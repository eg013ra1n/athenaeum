# Collaboration v3 — filter mapping (§6.2 built out)

Date: 2026-09-28. Status: approved by the owner in conversation (design),
spec written the same day. Amends
`2026-09-23-collab-v3-per-frame-model-design.md` §6.2 (R8) — recorded there
as amendment A7. Two repositories: the app (`athenaeum`) and the hub with
its portal (`athenaeum-hub`, portal under `athenaeum-hub/portal/` — the
sibling `athenaeum-hub-portal/` checkout is a stale copy and is never
edited).

## 1. Why

The owner's wave-3 smoke (open-items, "Collab v3 wave 3 — owner smoke
(2026-09-28)") found that a frame without a `FILTER` header cannot be
published: the gate fails it with `filter "" is not in the project
dictionary` and nothing in the app can change that. The same wall stops
every foreign spelling. The dev catalog has all of them among its LIGHT
frames:

| Raw `FILTER` | Where | Frames |
| ---- | ---- | ---- |
| absent | ATR2600M (mono), QHY268M-a137314 (mono), ASI183MM Pro (mono) | 891 · 12 · 79 |
| absent | ASI2600MC Pro / Duo / Air, ATR2600C (OSC, `RGGB`) | 4353 |
| `Slot 0` … `Slot 6`, `Filter#1`, `Filter#2` | QHY268M, ASI2600MM Pro | 123 |
| `1` … `7` | ASI294MM Pro, ASI6200MM Pro | 554 |
| `H`, `O`, `S` | QHY268M, ASI294MM Pro, ASI183MM Pro | 4 228 |

Wave 2 matched the trimmed `FILTER` against the dictionary's canonical
spellings and aliases only (plan ruling P3, "the normaliser, the
`filter_mappings` table and the modal belong to wave 3"); wave 3 became the
live exchange and never built them. This spec builds §6.2 as R8 always
meant it: the publisher confirms a mapping once per (camera, raw name), the
gate reads that mapping, and the coordinator can grow the vocabulary.

## 2. Owner rulings

- **F1 (2026-09-28) — an unset `FILTER` is a frame state, not a camera
  property.** OSC cameras shoot with or without a filter; mono cameras
  without a wheel shoot without one. Nothing may be inferred from
  `BAYERPAT`: there is no `channel = "osc"` special case and no OSC-derived
  canonical. The empty name is one more unmapped raw name and the publisher
  picks its canonical in the mapping modal. (Also a standing decision in
  open-items.)
- **F2 — the mapping is the publisher's, per account.** One row per
  (account, camera, raw name), remembered across sign-out and shared by
  every project the account publishes into. A project whose dictionary lacks
  the mapped canonical shows that as its own gate reason (§5.1, reason 3):
  the coordinator adds the canonical, or the publisher remaps. There is no
  per-project override.
- **F3 — a proposal is never a write.** The normaliser and the "sole
  unfiltered entry" default only preselect a `Select` in the modal; a
  mapping row exists only after the publisher saves it.
- **F4 — the dictionary gains one default entry for unfiltered frames**:
  canonical `None`, kind `unfiltered`. It reaches existing projects through a
  NEW hub migration; migration 0022 and the wave-1 seed constant stay
  byte-equal (their pin test is untouched).

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
(`api::account`), which is known from the first sign-in on and survives
sign-out and a hub `401` — the live session's account id is learned only at
the first hello and cleared at sign-out, so it cannot key a table the modal
writes before the runtime is up. Rows are never deleted at sign-out; a later
sign-in by the same e-mail finds them, a different e-mail sees none.

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
   The single letters `h`, `o`, `s` come from the dev catalog (4 228 frames);
   they are proposals, so a wrong guess costs one click.
5. The synonym's target must exist in the project's dictionary as a
   canonical (case-insensitive compare, the dictionary's spelling returned);
   otherwise fall back to `match_filter(remainder, dictionary)`; otherwise no
   proposal.
6. An empty raw name proposes the dictionary's `unfiltered`-kind entry when
   there is exactly one, else nothing.

Slot names (`Slot 0`, `Filter#1`, `1` … `7`) get no proposal. `UV/IR cut`
and similar OSC glass get none either — it is not `L` and not "no filter".
The `INSTRUME` display alias of §6.2 is out of scope (§9).

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
  project whose LATEST dictionary version has no entry of kind `unfiltered`,
  no canonical equal to `None` case-insensitively and none of the five
  aliases (case-insensitive, across all entries), insert version `MAX+1`
  with `latest.entries || '<UNFILTERED_ENTRY_JSON>'::jsonb`, `created_by` =
  `projects.created_by`, and `UPDATE projects SET version = version + 1`
  for those projects. A project that fails the guard is skipped — its
  coordinator adds the entry in the portal editor. Idempotent: a replay
  finds the kind present and changes nothing. A new pin test reads the
  migration file with `include_str!` and asserts its embedded literal
  parses to `UNFILTERED_ENTRY_JSON`. The version bump is what makes every
  app's next hello (`feed.rs` sends `version: p.hub_version` per project)
  reload the project and its dictionary — a migration cannot publish a feed
  event.
- `projects::project_page` coverage keeps its `unwrap_or("broadband")` for
  an unknown kind; the portal `CoverageFilter.kind` union gains
  `'unfiltered'` and renders it like the others (no new visual).
- Announce and the coordinator remap (`PATCH …/frames/{uuid}
  filterCanonical`) are unchanged: they validate against the current
  dictionary and `None` is simply one more canonical.

### 4.2 `PUT /projects/{id}/dictionary` — two refusals

Mirroring the thresholds fix of the same smoke (hub `f2c1385`):

- **409 `entries are identical to the current version`** when the submitted
  entries equal the current version's jsonb (after alias trimming). Nothing
  is minted, no version bump, no event.
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
disabled while the draft equals the stored entries** ("No changes to save.")
or has an error. One row per entry: `Canonical` (text, the hub's regex
mirrored client-side), `Kind` (`Select` over the four kinds), `Aliases`
(text, comma-separated, trimmed, empties dropped), `Remove`; `Add entry`
appends an empty row; duplicate canonicals (case-insensitive) and duplicate
aliases across the dictionary are flagged inline. A `409` from the hub is
shown as a `Note` with the hub's sentence. The version history renders the
way the threshold history does. Types `DictionaryEntry` / `DictionaryView`
join `types.ts`. Tests: a `DictionaryEditor.test.tsx` in the
`ThresholdEditor.test.tsx` style, and `Admin.test.tsx` gains the
"thresholds.edit sees the editor, members.manage does not" cases for the
dictionary (`GET /api/v1/projects/{id}/dictionary` is asked only when the
capability is held).

## 5. App

### 5.1 Gate reasons

`collab::gate::evaluate_frame` receives the resolution (§3.2) instead of
`filter_canonical: Option<String>` and writes one of:

1. `no FILTER header — needs a filter mapping` (`Unmapped`, empty raw);
2. `filter "Slot 0" needs a filter mapping` (`Unmapped`, non-empty raw);
3. `filter "H" is mapped to "Hb", which is not in this project's dictionary`
   (`MappedToMissing`).

The wave-2 sentence `filter "…" is not in the project dictionary` is retired
(its test `unmapped_filter_fails_with_its_name` becomes the three above).
The `fake_hub` keeps the hub's own announce wording — that is the hub's
sentence, not the gate's.

### 5.2 Gate report and commands

- `GateReport` gains `unmapped_filters: Vec<UnmappedFilter>` — one entry per
  distinct (instrume, filter_raw) among the candidate frames whose
  resolution is `Unmapped` or `MappedToMissing`, with `frames: i64`. Ordered
  by frame count descending, then instrume, then raw name. `GateReport` is
  already in the `ts_export` registry; `UnmappedFilter` joins it.
- **`get_collab_filter_mapping_sheet { projectId } → FilterMappingSheet`**:
  `dictionary: Vec<DictionaryEntry>` (the project's cached dictionary) and
  `rows: Vec<FilterMappingRow { instrume, filter_raw, frames, resolution:
  "mapped" | "mappedToMissing" | "matched" | "unmapped", canonical:
  Option<String>, proposal: Option<String> }>` over every distinct
  (instrume, filter_raw) of the project's candidate frames (the same
  `union_light_frames` set the gate walks), same order as above with the
  unresolved rows first. `Invalid("the project's filter dictionary has not
  been fetched yet")` when `dictionary_json` is NULL — the modal cannot
  offer a `Select` from nothing.
- **`set_collab_filter_mappings { projectId, mappings: [{ instrume,
  filterRaw, canonical: string | null }] }`**: upserts one row per item under
  the current account; `canonical` must be a canonical of the project's
  cached dictionary (`Invalid("\"Hb\" is not in the project dictionary")`
  otherwise, nothing written); `null` deletes the row ("back to automatic").
  One `BEGIN IMMEDIATE` transaction. On success it calls
  `collab_autopublish::request_auto_publish(Some(project_id))` — the
  "filter mapping changed" trigger of §5.2 of the per-frame spec — and
  returns the fresh `GateReport` so the page needs no second call.
- Both commands exist on both hosts (`commands/collab.rs`,
  `routes/collab.rs`), `#[tracing::instrument(skip_all, err)]`, registered
  in `invoke_handler![]` and `build_router`. `collab::filters` gains the
  DB-free `resolve_filter`/`propose_canonical`; `db::collab` gains
  `filter_mappings_for_account`, `upsert_filter_mapping`,
  `delete_filter_mapping`.

### 5.3 Project page

Next to `Publish N passing frames`: **`Map filters (K)`**, K =
`unmappedFilters.length`, shown only when K > 0, with the hint `K filter
names need a mapping before their frames can pass the gate`. It opens the
**Filter mapping** modal (the §6.2 modal), built on the page's existing
dialog pattern (`LinkObjectDialog`): one row per sheet row —
`(no FILTER) · ATR2600M — 891 frames → [Select]`, `Slot 0 · QHY268M — 53
frames → [Select]`, `H · ASI294MM Pro — 1538 frames → [Select: Ha]` — the
`Select` listing the project's canonicals (label `canonical · kind`) plus
`— automatic —`, preselected with the current mapping, else the proposal,
else empty. Rows already resolved (`mapped`/`matched`) are listed below a
divider with their current value, so an earlier choice can be changed here
— there is no other mapping UI. `Save` is disabled while nothing changed;
it calls `set_collab_filter_mappings` with the changed rows only, replaces
the page's `GateReport` with the returned one, closes the modal, and
`notify()`s `Filter mappings saved · N frames now pass the gate` (toast).
A `Select` left empty on an unresolved row is simply not sent. The
`GateTable` needs no change: reasons 1–3 already render in its `Gate`
column.

Auto-publish never opens the modal: an unresolved frame just does not pass,
and its reason is on the page (§5.1). The publish path itself is untouched —
`run_publish` keeps skipping frames that fail the gate.

## 6. Edge cases

- **Dictionary version moves and a mapped canonical disappears** → the
  frame's reason is 3 and the sheet lists the row as `mappedToMissing` with
  its stale canonical; the coordinator re-adds it or the publisher picks
  another. No automatic re-mapping.
- **Same raw name on two cameras** → two rows, two questions (R8: per
  (camera, filter name)); the proposal preselects both.
- **`INSTRUME` absent** → `instrume = ''`, one row per raw name for the
  camera-less frames.
- **Two projects, different vocabularies** → F2: one mapping per account;
  reason 3 names the missing canonical in the second project.
- **Coordinator remap on the hub** (`PATCH filterCanonical`) → a manifest
  edit (R8); it never touches the publisher's local mapping, and the next
  version of that frame carries the publisher's `ATH_FILT` again — the
  remap is re-applied by the coordinator if they still disagree. Unchanged
  behaviour, now stated.
- **A project without a cached dictionary** → the sheet command refuses
  (§5.2); the `Map filters` button is shown but the modal reports the
  refusal inline. The gate keeps failing closed (P3).
- **Mapping written, then the frame's `FILTER` header is edited in the
  catalog** (bulk metadata edit) → the frame resolves under its new raw name;
  the old row stays and is harmless.
- **Migration guard skips a project** (a custom alias `none` already
  exists) → the coordinator adds the entry by hand; the hub logs nothing
  (SQL), the plan's migration test pins the skip.

## 7. Testing

- **App, unit**: `filters.rs` — the four resolutions with precedence
  (mapping over alias, empty raw is `Unmapped`, `MappedToMissing` when the
  dictionary lacks it); the normaliser table (each synonym, the `nm`
  suffix, the vendor tokens, slot names → none, empty raw → the sole
  `unfiltered` entry, two `unfiltered` entries → none, a synonym whose target
  the dictionary lacks → alias fallback → none). `gate.rs` — the three
  reasons verbatim.
- **App, api** (fake hub, in-memory catalog, as `api/collab.rs` tests do):
  the sheet's rows and ordering; `set_…` refuses a canonical outside the
  dictionary and writes nothing; `null` deletes; the returned `GateReport`
  shows the frames passing; the auto-publish dirty mark is set; the account
  scoping (rows of another e-mail are invisible); the gate resolves through
  the mapping on the next `evaluate_collab_gate`. `tests/ts_contract.rs`
  covers the new types (run ALL targets — `cargo test -p athenaeum-core`).
- **App, e2e**: the existing three-instance live e2e gains one frame
  without `FILTER` that is unpublishable until a mapping to `None` is saved
  and then lands on the receivers with `ATH_FILT = 'None'`.
- **Hub**: `tests/dictionary.rs` — a new project has eight entries ending in
  `None`/`unfiltered`; announce with `None` passes; the 0026 pin; the
  backfill against a project seeded with the seven (a fixture inserted
  directly) adds version 2 and bumps `projects.version`, a replay changes
  nothing, a project holding alias `none` is skipped; PUT identical → 409;
  PUT removing a canonical in use → 409 with the count; PUT removing an
  unused canonical → OK; `kind: "unfiltered"` accepted, `kind: "osc"`
  refused. Whole suite green before any push (owner lesson 2026-09-18/20).
- **Portal**: `DictionaryEditor.test.tsx` (render, add/remove, Save gating,
  inline duplicates, 409 note) and the two `Admin.test.tsx` capability
  cases. `npm test` in `athenaeum-hub/portal`.

## 8. Acceptance (owner smoke, dev catalog, test hub)

1. Hub deployed to the test hub with 0026; portal Admin → Filter dictionary
   shows eight entries for an existing project; Save disabled untouched; add
   `Dualband` (narrowband) → v+1; remove `L` while frames use it → the 409
   note.
2. App: link the ATR2600M set → `Map filters (1)` → modal `(no FILTER) ·
   ATR2600M — 891 frames → None` preselected → Save → gate rows pass the
   filter precondition → Publish lands frames with `ATH_FILT = 'None'` on
   the second instance.
3. Link a QHY268M set with `Slot 0` and `H` → two rows, `H` preselects `Ha`,
   `Slot 0` empty → pick → Save → both resolve.
4. Link an ASI2600MC set (OSC, no `FILTER`) → the same `(no FILTER)` row for
   that camera → `None` → publishes; nothing about `BAYERPAT` anywhere (F1).
5. Sign out, sign in with the same e-mail → mappings still there.

## 9. Out of scope

- The `INSTRUME` display alias of §6.2 (its own small cycle).
- A per-project mapping override (F2 rules it out for now).
- Surfacing "K filter names need mapping" in `CollabAttention` for
  auto-publish users who never open the project page — candidate follow-up
  once the page flow has been smoked.
- Any change to the hub's announce/remap rules, to `ATH_FILT`'s meaning, or
  to how stacking groups by canonical.

## 10. Plans

Two plans, independent, may run in parallel; the hub deploy precedes the
app smoke because the modal needs `None` to exist on the test hub:

- `docs/superpowers/plans/2026-09-28-collab-v3-filter-mapping-hub-portal-plan.md`
  — §4 (entry + migration 0026 + PUT refusals + portal editor).
- `docs/superpowers/plans/2026-09-28-collab-v3-filter-mapping-app-plan.md`
  — §3 and §5 (table, resolution, normaliser, gate reasons, two commands on
  both hosts, modal), with the fake hub's default dictionary extended by the
  same `None` entry.
