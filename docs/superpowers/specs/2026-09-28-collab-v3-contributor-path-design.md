# Collaboration v3 — the contributor path (filter mapping, attestation, gate actions, the frame set's Project block)

Date: 2026-09-28. Status: approved by the owner in conversation (design),
spec written the same day; scope widened from "filter mapping" to the whole
contributor path on the owner's word after the process audit of the same
day. Amends `2026-09-23-collab-v3-per-frame-model-design.md` §6.1, §6.2
(R8), R3 and §8.2 — recorded there as amendment A7. Two repositories: the
app (`athenaeum`) and the hub with its portal (`athenaeum-hub`, portal
under `athenaeum-hub/portal/`; the sibling `athenaeum-hub-portal/`
checkout is a stale copy and is never edited).

## 1. Why

The owner's wave-3 smoke (open-items, "Collab v3 wave 3 — owner smoke
(2026-09-28)") found that a frame without a `FILTER` header cannot be
published: `filter "" is not in the project dictionary`, and nothing in
the app can change that. The process audit that followed traced the whole
contributor path through the code and found it reachable but narrow:

- Publishing works only when the app itself can calibrate the light —
  linked calibration sets with built masters. A set calibrated by an
  external tool, or a set with no calibration at all, is a dead end: R3
  (external attestation) was never built.
- The gate names a reason per frame (`no analysis`, `3 lights have no
  calibration links`, `no coordinates`, the filter sentence, a threshold)
  but offers no action: the user must guess where to fix it. §6.1 asks for
  "a row action where one exists (solve, map filter, attest)".
- From the frame set's own page nothing says the set is linked to a
  project, how its frames stand there, or how to link it; the only entry
  is the project page's "Link an object" (§8.2's Project block/column was
  wave-5 work).
- The "Auto-publish my frames" switch lives in the auto-replicate bar,
  which is shown only to roles that receive; a send-only member cannot
  turn auto-publish off.
- A dead hint ("Publishing calibrated lights from this device is not
  available in this version", decision C) still hangs on the Publish
  button's code path; it never fires but misleads the reader.

The filter finding is real data, not an edge: LIGHT frames of the dev
catalog by raw `FILTER`:

| Raw `FILTER` | Where | Frames |
| ---- | ---- | ---- |
| absent | ATR2600M (mono), QHY268M-a137314 (mono), ASI183MM Pro (mono) | 891 · 12 · 79 |
| absent | ASI2600MC Pro / Duo / Air, ATR2600C (OSC, `RGGB`) | 4353 |
| `Slot 0` … `Slot 6`, `Filter#1`, `Filter#2` | QHY268M, ASI2600MM Pro | 123 |
| `1` … `7` | ASI294MM Pro, ASI6200MM Pro | 554 |
| `H`, `O`, `S` | QHY268M, ASI294MM Pro, ASI183MM Pro | 4 228 |

Wave 2 matched the trimmed `FILTER` against canonical spellings and aliases
only (plan ruling P3); wave 3 became the live exchange. This spec builds
§6.2 as R8 meant it, builds R3, gives the gate its actions, and gives the
frame set its Project block — the pieces of waves 4 and 5 that the
contributor path cannot do without. The plate-solve precondition of §6.1
and the zero point (R9) stay deferred (§12).

## 2. Owner rulings

- **F1 (2026-09-28) — an unset `FILTER` is a frame state, not a camera
  property.** OSC cameras shoot with or without a filter; mono cameras
  without a wheel shoot without one. Nothing may be inferred from
  `BAYERPAT`: no `channel = "osc"` special case, no OSC-derived canonical.
  The empty name is one more unmapped raw name and the publisher picks its
  canonical in the mapping modal. (Also a standing decision in open-items.)
- **F2 — the mapping is the publisher's, per account.** One row per
  (account, camera, raw name), remembered across sign-out and shared by
  every project the account publishes into. A project whose dictionary
  lacks the mapped canonical shows that as its own gate reason (§5.1,
  reason 3): the coordinator adds the canonical, or the publisher remaps.
  No per-project override.
- **F3 — a proposal is never a write.** The normaliser and the "sole
  unfiltered entry" default only preselect a `Select`; a mapping row exists
  only after the publisher saves it.
- **F4 — the dictionary gains one default entry for unfiltered frames**:
  canonical `None`, kind `unfiltered`, reaching existing projects through a
  NEW hub migration; migration 0022 and the wave-1 seed constant stay
  byte-equal.
- **F5 — attestation is a frame-set flag, and an attested light is seeded
  in place** (R3 as amended by A1): no copy, no stamps, the row's path is
  the original's path. The app never calibrates an attested light for a
  project. The flag is the user's word — the app checks nothing about the
  pixels.
- **F6 — every gate reason that has a remedy in the app names it.** The
  gate report carries the blockers grouped by cause with the action that
  clears each; the project page renders one button per cause.
- **F7 — the auto-publish switch belongs to contributing, not receiving.**
  It sits in the Contribute tab for every member; the auto-replicate bar
  keeps only auto-download.
- **F8 — the manifest tells the coordinator how a frame was calibrated.**
  `meta.calibration = { dark, flat, bias, external }` is informational: no
  gate change, no threshold. Whether a project may refuse a light without
  a flat is a later ruling; the data to decide it lands now.

# Part I — Filter mapping (§6.2)

## 3. Model

### 3.1 App table

```sql
CREATE TABLE IF NOT EXISTS collab_filter_mappings (
    account     TEXT NOT NULL,   -- the signed-in e-mail (keys::ACCOUNT_EMAIL), lower-cased
    instrume    TEXT NOT NULL,   -- frames.instrume trimmed, '' when the header is absent
    filter_raw  TEXT NOT NULL,   -- frames.filter trimmed, '' when the header is absent
    canonical   TEXT NOT NULL,   -- a dictionary canonical, verbatim spelling
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (account, instrume, filter_raw)
);
```

`account` is the e-mail the sign-in wrote under `keys::ACCOUNT_EMAIL`
(`api::account`), known from the first sign-in on and surviving sign-out
and a hub `401` — the live session's account id is learned only at the
first hello and cleared at sign-out, so it cannot key a table the modal
writes before the runtime is up. Rows are never deleted at sign-out; a
later sign-in by the same e-mail finds them, a different e-mail sees none.

### 3.2 Resolution order (the gate)

`collab::filters::resolve_filter(raw, instrume, mappings, dictionary)`
returns one of:

1. **`Mapped(canonical)`** — a mapping row exists and its `canonical` is a
   canonical of the project's current dictionary (exact, case-sensitive —
   the hub's `filterCanonical` rule).
2. **`MappedToMissing(canonical)`** — a mapping row exists but the project's
   dictionary has no such canonical (the dictionary moved, or this is a
   second project with a smaller vocabulary).
3. **`Matched(canonical)`** — no mapping row; the trimmed raw name equals a
   canonical or an alias case-insensitively (today's `match_filter`, plan
   ruling P3). The dictionary's own spelling is returned, never the raw one.
4. **`Unmapped`** — none of the above. An empty raw name without a mapping
   row is always here (F1).

An explicit mapping wins over an alias hit because the publisher may have
meant it (a camera whose `L` slot holds a different glass). Only `Mapped`
and `Matched` feed `GateIdentity.filter_canonical` and `ATH_FILT`; the
file's `FILTER` header is never rewritten (§6.2).

### 3.3 Normaliser (proposal only, F3)

`collab::filters::propose_canonical(raw, dictionary) -> Option<String>`:

1. Lower-case, trim, collapse internal whitespace to one space.
2. Drop a trailing bandwidth token `\d+(\.\d+)?\s*nm` (`ha 3nm` → `ha`).
3. Drop the tokens `filter`, `astronomik`, `baader`, `optolong`, `antlia`,
   `chroma`, `zwo`, `svbony` wherever they occur as whole tokens (split on
   space, `-`, `_`).
4. Look the remainder up in the fixed synonym table:
   `l | lum | luminance | clear → L`; `r | red → R`; `g | green → G`;
   `b | blue → B`; `ha | h | h-alpha | halpha | h_alpha | hα | h-a → Ha`;
   `oiii | o3 | o | o-iii | o_iii → OIII`; `sii | s2 | s | s-ii | s_ii → SII`.
   The single letters `h`, `o`, `s` come from the dev catalog (4 228
   frames); they are proposals, so a wrong guess costs one click.
5. The synonym's target must exist in the project's dictionary as a
   canonical (case-insensitive compare, the dictionary's spelling
   returned); otherwise fall back to `match_filter(remainder, dictionary)`;
   otherwise no proposal.
6. An empty raw name proposes the dictionary's `unfiltered`-kind entry when
   there is exactly one, else nothing.

Slot names (`Slot 0`, `Filter#1`, `1` … `7`) get no proposal. `UV/IR cut`
and similar OSC glass get none either — it is not `L` and not "no filter".
The `INSTRUME` display alias of §6.2 is out of scope (§12).

## 4. Hub and portal

### 4.1 Dictionary entry for unfiltered frames (F4)

- `routes::dictionary::KINDS` gains `unfiltered`.
- A second constant `UNFILTERED_ENTRY_JSON` =
  `{"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}`.
  `default_dictionary()` returns `DEFAULT_DICTIONARY_JSON`'s seven entries
  followed by this one, so a new project's version 1 has eight entries.
  `DEFAULT_DICTIONARY_JSON` itself is unchanged, and so is
  `tests/dictionary.rs::migration_backfill_json_matches_default_dictionary`.
- **Migration 0026** (`0026_unfiltered_dictionary_entry.sql`): for every
  project whose LATEST dictionary version has no entry of kind
  `unfiltered`, no canonical equal to `None` case-insensitively and none of
  the five aliases (case-insensitive, across all entries), insert version
  `MAX+1` with `latest.entries || '<UNFILTERED_ENTRY_JSON>'::jsonb`,
  `created_by` = `projects.created_by`, and `UPDATE projects SET version =
  version + 1` for those projects. A project that fails the guard is
  skipped — its coordinator adds the entry in the portal editor.
  Idempotent: a replay finds the kind present and changes nothing. A new
  pin test reads the migration file with `include_str!` and asserts its
  embedded literal parses to `UNFILTERED_ENTRY_JSON`. The version bump is
  what makes every app's next hello (`feed.rs` sends `version:
  p.hub_version` per project) reload the project and its dictionary — a
  migration cannot publish a feed event.
- `projects::project_page` coverage keeps its `unwrap_or("broadband")` for
  an unknown kind; the portal `CoverageFilter.kind` union gains
  `'unfiltered'` and renders it like the others (no new visual).
- Announce and the coordinator remap (`PATCH …/frames/{uuid}
  filterCanonical`) validate against the current dictionary as before;
  `None` is simply one more canonical. **One rule changes** (found in
  execution, 2026-09-28): announce accepted `filterRaw` only when
  `1..=80` chars, so a header-less frame could never be announced. It now
  accepts `0..=80` after trimming (stored `""`); the app's mirror of the
  rule (`api::collab::hub_frame_rule_problem`) and its fake hub follow.

### 4.2 `PUT /projects/{id}/dictionary` — two refusals

Mirroring the thresholds fix of the same smoke (hub `f2c1385`):

- **409 `entries are identical to the current version`** when the
  submitted entries equal the current version's jsonb (after alias
  trimming). Nothing is minted, no version bump, no event.
- **409 `canonical "X" is used by N frames — remap them first`** when a
  canonical of the current version is absent from the submitted entries
  (case-sensitive; a rename is a removal plus an addition) and
  `project_frames` rows of this project carry it. Aliases may be removed
  freely — they never reach `filter_canonical`.

Validation otherwise unchanged (1..=50 entries, canonical
`^[A-Za-z0-9][A-Za-z0-9_-]{0,15}$`, ≤ 20 aliases of 1..=40 chars, aliases
unique across the dictionary, kind ∈ the four).

### 4.3 Portal dictionary editor

Under Admin, visible to `thresholds.edit` (coordinator implicitly), mounted
right after `ThresholdEditor` with the same contract: seeded once from the
stored entries, re-mounted by a `key` on the dictionary version, **Save
disabled while the draft equals the stored entries** ("No changes to
save.") or has an error. One row per entry: `Canonical` (text, the hub's
regex mirrored client-side), `Kind` (`Select` over the four kinds),
`Aliases` (text, comma-separated, trimmed, empties dropped), `Remove`;
`Add entry` appends an empty row; duplicate canonicals (case-insensitive)
and duplicate aliases across the dictionary are flagged inline. A `409`
from the hub is shown as a `Note` with the hub's sentence. The version
history renders the way the threshold history does. Types
`DictionaryEntry` / `DictionaryView` join `types.ts`. Tests: a
`DictionaryEditor.test.tsx` in the `ThresholdEditor.test.tsx` style, and
`Admin.test.tsx` gains the "thresholds.edit sees the editor, members.manage
does not" cases for the dictionary (`GET /api/v1/projects/{id}/dictionary`
is asked only when the capability is held).

## 5. App — mapping in the gate and the modal

### 5.1 Gate reasons

`collab::gate::evaluate_frame` receives the resolution (§3.2) instead of
`filter_canonical: Option<String>` and writes one of:

1. `no FILTER header — needs a filter mapping` (`Unmapped`, empty raw);
2. `filter "Slot 0" needs a filter mapping` (`Unmapped`, non-empty raw);
3. `filter "H" is mapped to "Hb", which is not in this project's
   dictionary` (`MappedToMissing`).

The wave-2 sentence `filter "…" is not in the project dictionary` is
retired (its test `unmapped_filter_fails_with_its_name` becomes the three
above). The `fake_hub` keeps the hub's own announce wording — that is the
hub's sentence, not the gate's.

### 5.2 Commands

- **`get_collab_filter_mapping_sheet { projectId } → FilterMappingSheet`**:
  `dictionary: Vec<DictionaryEntry>` (the project's cached dictionary) and
  `rows: Vec<FilterMappingRow { instrume, filter_raw, frames, resolution:
  "mapped" | "mappedToMissing" | "matched" | "unmapped", canonical:
  Option<String>, proposal: Option<String> }>` over every distinct
  (instrume, filter_raw) of the project's candidate frames (the same
  `union_light_frames` set the gate walks), unresolved rows first, then by
  frame count descending, instrume, raw name. `Invalid("the project's
  filter dictionary has not been fetched yet")` when `dictionary_json` is
  NULL — the modal cannot offer a `Select` from nothing.
- **`set_collab_filter_mappings { projectId, mappings: [{ instrume,
  filterRaw, canonical: string | null }] }`**: upserts one row per item
  under the current account; `canonical` must be a canonical of the
  project's cached dictionary (`Invalid("\"Hb\" is not in the project
  dictionary")` otherwise, nothing written); `null` deletes the row ("back
  to automatic"). One `BEGIN IMMEDIATE` transaction. On success it calls
  `collab_autopublish::request_auto_publish(Some(project_id))` — the
  "filter mapping changed" trigger of §5.2 of the per-frame spec — and
  returns the fresh `GateReport` so the page needs no second call.
- Both on both hosts (`commands/collab.rs`, `routes/collab.rs`),
  `#[tracing::instrument(skip_all, err)]`, registered in
  `invoke_handler![]` and `build_router`. `collab::filters` gains the
  DB-free `resolve_filter`/`propose_canonical`; `db::collab` gains
  `filter_mappings_for_account`, `upsert_filter_mapping`,
  `delete_filter_mapping`.

### 5.3 The Filter mapping modal

Opened by the `Map filters` blocker button (§7.2), built on the page's
existing dialog pattern (`LinkObjectDialog`): one row per sheet row —
`(no FILTER) · ATR2600M — 891 frames → [Select]`, `Slot 0 · QHY268M — 53
frames → [Select]`, `H · ASI294MM Pro — 1538 frames → [Select: Ha]` — the
`Select` listing the project's canonicals (label `canonical · kind`) plus
`— automatic —`, preselected with the current mapping, else the proposal,
else empty. Rows already resolved (`mapped`/`matched`) are listed below a
divider with their current value, so an earlier choice can be changed
here — there is no other mapping UI. `Save` is disabled while nothing
changed; it calls `set_collab_filter_mappings` with the changed rows only,
replaces the page's `GateReport` with the returned one, closes the modal,
and `notify()`s `Filter mappings saved · N frames now pass the gate`
(toast). A `Select` left empty on an unresolved row is simply not sent.

Auto-publish never opens the modal: an unresolved frame does not pass, and
its reason is on the page.

# Part II — The rest of the contributor path

## 6. External attestation (R3, F5)

### 6.1 Model

`frames_set` gains `calibrated_externally INTEGER NOT NULL DEFAULT 0` and
`attested_at TEXT` (idempotent `ALTER TABLE … ADD COLUMN` in `init_db`,
the pattern every other added column uses). `FramesSet` (Rust and
`models.ts`) carries both.

**`set_frame_set_attestation { framesSetId, attested: bool }`** on both
hosts: writes the flag and the timestamp (or clears both), then
`request_auto_publish_for_sets(&[id])` — the "external-calibration
attestation set" trigger of §5.2 of the per-frame spec. It refuses
(`Invalid`) a set that is archived and zipped (`archived_at` set): its
files are not on disk to seed.

### 6.2 UI — the frame set's Calibration tab

A checkbox at the top of the Calibration tab, above the hierarchy:
**Calibrated by an external tool** with the help line `The lights of this
set are already calibrated (dark and flat applied) as single-channel frames
— mono or CFA, not debayered RGB. Collaboration projects take them as they
are; nothing in the app calibrates them again.` Turning it on when the set
has linked calibration sets asks once (`ConfirmDialog`): `This set has N
linked calibration sets. Attesting it means projects take the lights as
they are and ignore those masters. Attest?`. Turning it off needs no
confirmation. The header line of the set gains `· attested` after the
calibrated/uncalibrated counts while the flag is on.

### 6.3 Gate

`frame_cal_verdict` returns `Ok(())` for a frame whose set is attested —
before the export-readiness walk, so an attested set needs no calibration
links at all. The gate row's blocker list (§7) never contains
`linkCalibration`/`buildMasters` for such a frame.

### 6.4 Publish — seeded in place

In `run_publish`'s split, a candidate whose set is attested takes the
external branch:

- **Recipe** `external:<files.size>:<files.modified_at>` — zero I/O, and
  the scanner's in-place re-parse keeps both columns current, so an
  overwritten file (re-calibrated outside) changes the recipe and becomes
  `PublishKind::Update` on the next publish. `force` (Republish) behaves as
  today.
- **Target** = the original path (`files` row), for `New`, `Update` and
  `Adopt` alike; no landing name is picked, no `hub_file_name_problem`
  check on a name the app did not choose — the manifest `fileName` is the
  original's basename, checked by the hub's file-name rule as any other,
  with a refusal reason `the hub refuses this file name (...)` held back as
  today. Duplicate basenames within the publisher (two sessions' `Light_0001.fits`) are
  refused with `a frame named "…" is already published by you — rename the
  file` rather than renamed on disk (A1: the app never moves an attested
  original).
- **No generation**: attested plans skip `run_publish_generation` entirely
  (no compute permit is taken for them); `WrittenFrame.xxh3` comes from
  `package::xxh3_full_file` over the original, run in the same
  `spawn_blocking` as the generation. No `ATH_*` stamps, no `CALSTAT`, no
  WCS rewrite (F5/A1). `meta.calibration = { external: true, dark: false,
  flat: false, bias: false }` (F8); the manifest's WCS fields come from the
  plate solve as for any frame (`build_frame_meta`).
- **Seed** by reference from the original path (A1: reference import works
  across volumes); an update seeds the same path under the new version
  with no temp file and no rename — the file IS the original — so the
  update branch's disk-lock step reduces to the seed plus the own-row
  write.
- The own row's `landed_path` is the original path; disk truth, holder
  reports and the scanner's Collaboration-root reconcile read the table
  (A1) and are unchanged. A deleted original reads `Published · not on
  disk` (§8.2), never re-fetched.

`meta.calibration` for a generated frame (the existing branch) reports
which masters the `GenerationSpec` actually used: `dark`/`flat`/`bias`
true when the corresponding master path is present, `external: false`.

### 6.5 What attestation does NOT do

- It does not change the calibrated-lights export or the personal stacking
  of the set (R3's "never calibrated again by export or stacking" is a
  follow-up, §12); a project stack (wave 6) reads project frames and
  therefore never calibrates them.
- It does not inspect the pixels: a debayered RGB file attested by mistake
  announces as `mono` and is wrong on the receiving side. The confirm text
  says so; a `NAXIS3` check waits for the header blob to expose it.

## 7. Gate blockers and actions (F6)

### 7.1 Report

`GateReport` gains `blockers: Vec<GateBlocker { kind, frames: i64, sets:
Vec<i64>, names: Vec<UnmappedFilter> }>`, one per cause present, ordered
as below, derived from the rows' failures (each row may count in several).
`UnmappedFilter { instrume, filter_raw, frames }` is filled for
`mapFilter` only — one per distinct (instrume, raw name) among the rows
with a §5.1 reason — and empty otherwise. Both types join the `ts_export`
registry.

| `kind` | From failures | Action |
| ---- | ---- | ---- |
| `analyze` | `no analysis` | `analyze_frame_set` per set in `sets` |
| `solve` | `no coordinates`, `unknown pixel scale` | `plate_solve_batch` over the failing frames |
| `linkCalibration` | the readiness sentences with "no calibration links" / "No calibration is linked" | open the set's Calibration tab |
| `buildMasters` | the readiness sentences with "Build masters first" / "no master" / "master file missing" | open the set's Calibration tab |
| `attest` | any calibration reason, offered beside the two above | open the set's Calibration tab (the checkbox is there) |
| `mapFilter` | §5.1 reasons 1–3 | the Filter mapping modal (§5.3); `names` filled |
| `threshold` | any rule failure, `frame appears trailed` | none — informational |
| `uuid` | `frame has no uuid` | none — re-scan the set (message) |
| `outsideTarget` | `outside target radius` | none — informational |

The readiness sentences are matched by the same substrings the export tab
already keys on; a sentence none of them match counts under
`linkCalibration`.

### 7.2 UI — "What blocks publishing"

Above `GateTable`, when any blocker exists, a compact list — one line per
blocker with its frame count and one button:

- `79 frames have no analysis` **Analyze** (runs `analyze_frame_set` for
  each set, disabled while the analysis progress event for that set is
  live);
- `12 frames have no coordinates or pixel scale` **Solve 12 frames**;
- `891 frames are not calibrated` **Open calibration** (navigates to the
  set's Calibration tab; with several sets, a small menu of set names) and
  **Attest as calibrated…** (the same navigation, the line says the
  checkbox is there);
- `2 filter names need a mapping` **Map filters** (§5.3);
- `4 frames fail a threshold`, `3 frames are outside the target` — text
  only.

After an action the page re-evaluates the gate (`evaluate_collab_gate`)
when the corresponding completion event arrives (`analysis-complete`,
`plate-solve-complete`, the existing listener pattern) or on return to the
page. `GateTable` keeps the per-row reason text; no per-row buttons.

The decision-C hint and its `publishBlockedByCalibration` predicate are
removed; the Publish button's tooltip is `No passing frames to publish yet`
when `publishable == 0`.

## 8. The frame set's Project block and column (§8.2)

### 8.1 Derivation — one source

`collab::contributor_state::derive(conn, project, frame_ids) ->
Vec<(frame_id, ContributorState)>` implements §3.3 of the per-frame spec
for the contributor's own frames, and is the ONE derivation both pages
use:

| State | When |
| ---- | ---- |
| `notPublished` | no own row and the gate row is publishable |
| `failsGate(reason)` | no own row and the gate row fails (first reason) |
| `pendingApproval` | own row `pending` |
| `published(vN)` | own row `published`, recipe equals the row's `recipe_hash` |
| `updatePending` | own row `published`/`pending`, current recipe ≠ the row's |
| `rejected(reason)` | own row `rejected` |
| `publishedNotOnDisk` | own row published, `landed_path` missing on disk (the row's `on_disk` as `list_collab_frames` already computes it) |
| `publishedNowFailsGate(reason)` | own row published, recipe unchanged, the gate row fails now (thresholds are prospective — informational) |

The recipe is `recipe_hash_of_inputs` (or the external recipe, §6.4) —
catalog reads only, no pixel I/O, so the derivation is cheap enough for a
page load.

### 8.2 Command

**`get_frame_set_project_status { framesSetId } → FrameSetProjectStatus`**
on both hosts:

```
{
  links: [{ projectId, slug, title, publishingHere, autoPublish,
            counts: { notPublished, failsGate, pendingApproval, published,
                      updatePending, rejected, publishedNotOnDisk,
                      publishedNowFailsGate },
            frames: [{ frameId, state, reason: string | null }] }],
  candidates: [{ projectId, slug, title, distanceDeg }]   // joined, within radius, not linked
}
```

`candidates` is `find_matching_projects` (already the source of the
`project-set-match` notification). Signed out → `links` and `candidates`
empty, no error (the block then says nothing about projects).

### 8.3 UI

- **Block**, in the set header under the frames/calibrated/sessions line.
  Linked: `Project <title> · 812 published · 79 fail gate · 3 update
  pending` with **Open project** (→ `/projects/<id>`, Contribute tab);
  several projects → one line each. Not linked, with candidates: `Matches
  project <title> (0.4° away)` with **Link to project** (`set_collab_link`
  and reload) — the same action the `project-set-match` notification
  offers. Not linked, no candidates: nothing beyond the existing **Publish
  as project** button.
- **Column** `Project` in the set's lights table (the analysis view's
  table): a chip per frame with the state's short label (`—`, `fails gate`,
  `pending`, `v3`, `update`, `rejected`, `not on disk`, `now fails`) and the
  reason as its title; hidden when the set is linked to no project.
- The project page's own-frames table (`PublicationHistory`) shows the same
  derived state chip next to the hub state, so the two views can never
  disagree.

## 9. The auto-publish switch (F7)

`AutoReplicateBar` keeps **Auto-download contributions** and the published
volume; **Auto-publish my frames** moves into the Contribute tab's header
row (beside `Linked objects`), rendered for every member, wired to
`set_project_auto_publish` as today. `AutoReplicateBar.test.tsx` loses the
auto-publish case and the Contribute tab gains it.

## 10. Edge cases

- **Dictionary version moves and a mapped canonical disappears** → reason
  3, the sheet lists the row as `mappedToMissing`; the coordinator re-adds
  or the publisher picks another. No automatic re-mapping.
- **Same raw name on two cameras** → two rows, two questions (R8: per
  (camera, filter name)); the proposal preselects both.
- **`INSTRUME` absent** → `instrume = ''`, one row per raw name.
- **Two projects, different vocabularies** → F2: one mapping per account;
  reason 3 names the missing canonical in the second project.
- **Coordinator remap on the hub** → a manifest edit (R8); it never touches
  the publisher's local mapping, and the next version of that frame carries
  the publisher's canonical again. Unchanged behaviour, now stated.
- **A project without a cached dictionary** → the sheet command refuses;
  the `Map filters` button is shown and the modal reports the refusal
  inline. The gate keeps failing closed (P3).
- **Mapping written, then the frame's `FILTER` header edited in the
  catalog** → the frame resolves under its new raw name; the old row stays.
- **Migration guard skips a project** → the coordinator adds the entry by
  hand; the plan's migration test pins the skip.
- **Attested set, then un-attested** → its published frames keep their
  versions on the hub; the next publish sees the frames as `Update` (the
  generated recipe differs from the external one) and posts calibrated
  versions written into the publisher folder — the own row's path moves
  with the version, as any update does.
- **Attested set with an OSC light** → announces `channel = "osc"` from
  `BAYERPAT` as today, seeded as the CFA file it is (R21 `splitOsc =
  false`).
- **Attested set inside the Collaboration root** → allowed; the scanner
  reconciles the file against the table (A1) and finds its own row.
- **Publishing device bound elsewhere (A6)** → unchanged: attested new
  frames are held back like any new frame.
- **Blocker "Solve" on frames of several sets** → one `plate_solve_batch`
  over all failing frame ids; the button is disabled while a solve run is
  live.

## 11. Testing

- **App, unit**: `filters.rs` — the four resolutions with precedence, the
  normaliser table (each synonym, the `nm` suffix, vendor tokens, slot
  names → none, empty raw → the sole `unfiltered` entry, two → none,
  target missing → alias fallback → none); `gate.rs` — the three filter
  reasons verbatim, the blocker derivation table (every kind, a row
  counted under two kinds, `attest` offered beside the calibration kinds);
  `contributor_state.rs` — every row of §8.1's table.
- **App, api** (fake hub, in-memory catalog, as `api/collab.rs` tests do):
  the sheet's rows and ordering; `set_…` refuses a canonical outside the
  dictionary and writes nothing; `null` deletes; the returned `GateReport`
  shows the frames passing; the dirty mark is set; account scoping; the
  attestation command flips the verdict and marks the set dirty, refuses a
  zipped set; an attested candidate's plan has the original path as target
  and no generation spec, its announce carries `meta.calibration.external
  = true` and the file's real `xxh3`/`byteSize`; a generated frame's
  `meta.calibration` lists its masters; an overwritten attested file (size
  bumped) becomes `Update`; `get_frame_set_project_status` counts and
  candidates. `tests/ts_contract.rs` covers every new type (run ALL
  targets — `cargo test -p athenaeum-core`).
- **App, e2e**: the existing three-instance live e2e gains one frame
  without `FILTER` that is unpublishable until a mapping to `None` is
  saved and then lands on the receivers with `ATH_FILT = 'None'`, and one
  attested set whose light lands on the receivers byte-identical to the
  original with no `ATH_*` cards.
- **App, frontend (Vitest)**: the blocker list renders one line per kind
  with the right button and calls the right command; the Filter mapping
  modal preselects and sends only changed rows; the Contribute tab shows
  the auto-publish switch for a send-only member; the set page's block in
  its three states and the column's chips.
- **Hub**: `tests/dictionary.rs` — a new project has eight entries ending
  in `None`/`unfiltered`; announce with `None` passes; the 0026 pin; the
  backfill against a project seeded with the seven (a fixture inserted
  directly) adds version 2 and bumps `projects.version`, a replay changes
  nothing, a project holding alias `none` is skipped; PUT identical → 409;
  PUT removing a canonical in use → 409 with the count; PUT removing an
  unused canonical → OK; `kind: "unfiltered"` accepted, `kind: "osc"`
  refused. Whole suite green before any push.
- **Portal**: `DictionaryEditor.test.tsx` (render, add/remove, Save
  gating, inline duplicates, 409 note) and the two `Admin.test.tsx`
  capability cases. `npm test` in `athenaeum-hub/portal`.

## 12. Out of scope

- The plate-solve precondition of §6.1 (header fallbacks P4 stand) and the
  zero point R9 (`ATH_ZP`, consumed by stacking — the stacking wave).
- The `INSTRUME` display alias of §6.2; a per-project mapping override
  (F2).
- Export and personal stacking of an attested set (R3's second half): the
  calibrated-lights export still calibrates it; a follow-up rules whether
  the mode is refused or passes the file through.
- A `NAXIS3` guard on attested files; a threshold on `meta.calibration`
  (F8 lands the data only).
- Surfacing blockers in `CollabAttention` for auto-publish users who never
  open the project page.

## 13. Acceptance (owner smoke, dev catalog, test hub)

1. Hub deployed to the test hub with 0026; portal Admin → Filter dictionary
   shows eight entries for an existing project; Save disabled untouched;
   add `Dualband` (narrowband) → v+1; remove `L` while frames use it → the
   409 note.
2. App: link the ATR2600M set → the blocker list says `2 filter names need
   a mapping` → **Map filters** → `(no FILTER) · ATR2600M — 891 frames →
   None` preselected → Save → the blocker line disappears → Publish lands
   frames with `ATH_FILT = 'None'` on the second instance.
3. QHY268M set with `Slot 0` and `H` → two rows, `H` preselects `Ha`, `Slot
   0` empty → pick → both resolve.
4. ASI2600MC set (OSC, no `FILTER`) → the same `(no FILTER)` row for that
   camera → `None` → publishes; nothing about `BAYERPAT` anywhere (F1).
5. A set with lights only (no calibration linked): the blocker list says
   `N frames are not calibrated` with **Open calibration** and **Attest as
   calibrated…**; on the set's Calibration tab tick **Calibrated by an
   external tool** → back on the project page the blocker is gone → Publish
   seeds the originals in place (the publisher folder stays empty for
   them) → the second instance receives them byte-identical; overwrite one
   original → the set page's column says `update` → Publish posts v2.
6. On the set page: the Project block shows the counts and **Open
   project**; a second, unlinked set inside the radius shows **Link to
   project**.
7. Sign in as a send-only member → the Contribute tab shows **Auto-publish
   my frames**; the dead "not available in this version" line is gone.
8. Sign out, sign in with the same e-mail → mappings still there.

## 14. Plans

Two plans, independent, may run in parallel; the hub deploy precedes the
app smoke because the modal needs `None` to exist on the test hub:

- `docs/superpowers/plans/2026-09-28-collab-v3-contributor-path-hub-portal-plan.md`
  — §4 (entry + migration 0026 + PUT refusals + portal editor).
- `docs/superpowers/plans/2026-09-28-collab-v3-contributor-path-app-plan.md`
  — Part I §3, §5 and Part II §6–§9, with the fake hub's default
  dictionary extended by the same `None` entry.
