# Collaboration v3 — the per-frame project model

Status: design, agreed with the owner in dialogue on 2026-09-23. Supersedes the
package-centred parts of the Stage II design
(`2026-07-06-collaboration-projects-design.md` §3 "Publication unit", §7
"catalog boundary"), the multi-source distribution design
(`2026-07-26-multi-source-project-distribution-design.md` §3.4 disk cost) and
the lifecycle note "the hub records no file names, no frames"
(`athenaeum-hub/docs/design/2026-09-19-collab-lifecycle-4-members.md`). Every
other rule in those documents stands unless this document says otherwise.

## 1. Why

The first real project will have about 78 members. Today:

- a member cannot publish at all — the project gate resolves every frame to
  `NotCalibrated` (calibrated-export-v2 §8a, decision C), so
  `publish_collab_frames` always fails with "no publishable frames";
- a processor pays two to three times the project size in disk (store-owned
  blobs + a plain copy in the landing folder + a hard-link seed that degrades
  to a copy across volumes);
- the hub knows packages, not frames, so nobody can see what the project holds
  without downloading it, coverage per filter is guessed from medians, a
  contributor's re-publish supersedes a whole package and resets its holders;
- the portal's quality-threshold editor is three unlabeled text boxes, a typo
  silently disables the rule for everyone, and `0.6` cannot be typed.

This design makes the **frame** the unit of everything (announcement, holding,
version, moderation, replication policy), stores one copy of every file on
every machine, puts a per-frame manifest on the hub, and gives the project a
real page in the app.

## 2. Owner rulings (2026-09-23)

Numbered so plans and reviews can cite them.

- **R1 — Project ≠ frame set.** A project is its own type with the visual
  hierarchy *object → publisher → camera → filter → …*. Its frames never
  become `frames` rows, never cluster, never join sessions or calibration
  matching. The WBPP export of a project uses that hierarchy.
- **R2 — Six tabs** in the app: Summary · My contribution · Lights · Stacking ·
  Export · History. Contribute/Receive/Moderation as separate tabs go away.
- **R3 — Externally calibrated sets.** A frame set can be attested "already
  calibrated (external tool)". Such lights pass the gate's calibration
  precondition, are never calibrated again by export or stacking, and are
  stamped `CALSTAT` with `ATH_CSRC = external` at publish.
- **R4 — Hub stores the per-frame manifest, file names included.** Pixels
  never. File names are needed to find abandoned or deleted files by eye; the
  app shows names, not uuids.
- **R5 — Frame is the unit.** Packages are removed from the model. Announce,
  hold, supersede, exclude, replicate: all per frame uuid. A *publication* is
  an event (History), not an entity.
- **R6 — One copy per machine.** Receivers land blobs by reference
  (`ExportMode::TryReference`), the collab blob store lives under the
  Collaboration root so the move never degrades to a copy; a contributor
  writes its calibrated file once into its own project folder and seeds it by
  reference.
- **R7 — Selective replication.** A processor's policy: filters, publishers,
  quality, byte budget. The policy is the truth; disk follows the policy.
- **R8 — Filter dictionary on the hub** (canonical names + aliases, versioned
  like thresholds); the contributor confirms the mapping once per (camera,
  filter name) at publish; the coordinator may remap later — a remap is a
  manifest edit, never a re-transfer.
- **R9 — Equivalent exposure by photometric zero point (ZP)**, computed for
  every solved light (project or personal), with the ladder ZP → t/N² from
  headers → seconds (strict, warned). Stacking groups project frames by
  canonical filter × exposure class.
- **R10 — Canonical output grid per project** (centre and radius from the
  project target, scale chosen by the coordinator, default the median of
  contributions); partial coverage is integrated with per-pixel N and a
  coverage map; WCS travels with the file, so plate-solve is a publish
  precondition.
- **R11 — Trust per publisher.** With approval on, a publisher's *first*
  publication is moderated; once trusted, every later frame that passes the
  gate is published without a click. Trust is revocable. The gate still runs on
  every frame.
- **R12 — Accepted frames are never withdrawn.** Every project frame carries
  `accepted` (default true when it passes the gate). A contributor deleting a
  file locally does not withdraw it: once seeded it belongs to the project.
  Only the coordinator (and delegates holding `data.moderate`) can set
  `accepted = false` — a manifest flag with a reason, never a delete on
  holders. There is no user-facing "withdraw everywhere" action.
- **R13 — Rejection only before seeding.** A moderated first publication can
  be rejected while it exists only on the publisher's and the coordinator's
  machines; after seeding, exclusion (R12) is the only tool.
- **R14 — Account deletion**: frames stay, the publisher name becomes
  "former member" everywhere. Closes backlog open question 3 and fixes the
  house-rules text.
- **R15 — The portal shows coverage and goals to everyone; the per-frame list
  by publisher is visible to members only.**
- **R16 — Auto-publish is on by default** for a linked frame set.
- **R17 — Deletion semantics** (§5.5, §10): a deleted *replica* is re-downloaded
  while the policy still wants it; a deleted *own* frame is not re-downloaded,
  shows "Published · not on disk", and offers "Ask to exclude". A mass loss
  pauses replication and asks. The guard thresholds live in
  Settings → Transfers.
- **R18 — A foreign file dropped into the Collaboration folder is inert**: not
  registered, not seeded, not stacked, not exported; the app lists it with
  Remove / Contribute (the normal path through a scan root, analysis, solve and
  gate).
- **R19 — Versioned polling.** One cheap "project versions" request every
  15 s; the full refresh only when a version moved. Retry cadence for failed
  downloads stays in minutes.
- **R20 — The portal threshold editor becomes dropdowns**, the metric/operator
  registry is validated on the hub, fractional values can be typed. This is
  wave 0 and does not wait for the rest.
- **R21 — OSC channel split and super luminance** are a separate stacking spec;
  this design only guarantees the manifest carries channel provenance.

## 3. Model

### 3.1 Entities

| Entity | Lives on | Identity | Notes |
| ---- | ---- | ---- | ---- |
| Project | hub | uuid | target (name, RA/Dec, radius), goals per canonical filter (seconds), filter dictionary version, thresholds version, canonical grid, options (`splitOsc`, `requireApproval`, join policy…), `version` counter |
| Member | hub | (project, account) | data role `send` / `send_receive`, governance caps, `trusted_publisher` flag |
| Project frame | hub + every member's app | `frame_uuid` (the source frame's `ATH_CSRC`) | one row per frame; see 3.2 |
| Frame version | hub | (`frame_uuid`, `content_version`) | BLAKE3 hash, byte size, `ATH_CVER`; a re-publish with new pixels bumps it |
| Holder | hub | (`frame_uuid`, device) | `reported_at`; fresh = reported within 75 min (H3 rule unchanged) |
| Publication | hub (`project_events`) | event id | "publisher X published N frames" — History only |
| Local project frame | app | (`project_id`, `frame_uuid`) | replaces `project_contributions` + `project_packages`; see 5.4 |

### 3.2 The project frame row (hub manifest)

Wire and storage, camelCase on the wire:

```
frameUuid, publisherAccountId, publisherSlug,
fileName            (validated by the same rule as landing: validate_rel_path)
contentVersion, blake3, byteSize, xxh3
filterRaw, filterCanonical, channel ("mono" | "osc-r" | "osc-g" | "osc-b" | "osc")
instrume, telescope, xbinning, naxis1, naxis2, pixelScaleArcsec, bayerpat
exptimeSec, dateObs, focalLen?, aperture?
fwhmArcsec, eccentricity, starsDetected, medianSnr, snrWeight, frameSnr
zeroPoint?  (ZP, mag for 1 ADU/s, per canonical filter; null when unsolved or no catalog stars)
wcs: { crval1, crval2, crpix1, crpix2, cd[4], sip? }   — enough to place the footprint on the canonical grid
gate: { thresholdsVersion, passed: true, failures: [] }
accepted: bool, acceptedReason?, acceptedBy?, acceptedAt?
state: "pending" | "published" | "rejected"
supersededBy?: contentVersion
manifestVersion: project version at which this row last changed
```

Size: about 400 bytes per row. A 10 000-frame project is a few MB.

### 3.3 Frame states

Contributor's view of its own frame (My contribution, and the *Project* column
on the frame set page — one derivation, one source):

`Not published` · `Pending approval` · `Published vN` · `Update pending`
(content changed locally, not yet announced) · `Fails gate` (reason) ·
`Rejected` (reason, pre-seed only) · `Published · not on disk` ·
`Published · now fails gate` (thresholds are prospective — informational).

Project-side (Lights): `Published` · `Excluded` (reason) · `Pending` (visible to
moderators only). Per device: `On disk` · `Not on disk` · `Downloading`.

## 4. Hub

### 4.1 Tables (migration 0022+)

- `project_frames` — the manifest row (3.2). Unique `(project_id, frame_uuid)`.
- `project_frame_versions` — `(project_id, frame_uuid, content_version, blake3, byte_size, xxh3, announced_at)`.
- `frame_holders` — `(project_id, frame_uuid, device_id, content_version, reported_at)`; replaces `have_reports`.
- `project_filter_dictionary` — `(project_id, version, entries jsonb)`; entries = `[{canonical, aliases[], kind: "broadband"|"narrowband"|"luminance"}]`.
- `project_members.trusted_publisher boolean not null default false`.
- `projects.version bigint not null default 0` — bumped by every change a device must see (membership, thresholds, dictionary, any manifest row, options). `membership_version` stays for the snapshot signature.
- `projects.canonical_grid jsonb` — `{scaleArcsec, widthPx, heightPx, crval1, crval2, rotationDeg}`; null until the coordinator sets it. The app's plan gate reports blocker `grid` with a proposed value (median scale of accepted frames, size from the target radius); the coordinator confirms it in the app or the portal.
- `package_announcements`, `have_reports` — dropped. No migration of rows: the test hub holds 0 published bytes and prod has had publishing blocked since 2026-08-31 (decision C). Verified before the drop by the deploy procedure (`SELECT count(*) FROM package_announcements`), refuse the migration if non-zero.

### 4.2 Routes (device token or portal session, `AuthAccount`)

| Route | Who | What |
| ---- | ---- | ---- |
| `GET /me/project-versions` | member | `[{projectId, version}]` for the caller's projects. Served from an in-process cache keyed by project id, invalidated on every bump; no DB hit on the hot path. **The 15 s poll hits only this.** |
| `GET /projects/{id}/manifest?since=V` | member | rows with `manifestVersion > V` plus tombstones; `since=0` = full. Paged (1000 rows). |
| `POST /projects/{id}/frames` | member with role `send`/`send_receive` | announce a batch of rows (up to 500). Validation: names via the landing path rule, `filterCanonical ∈ dictionary`, gate.passed with the current thresholds version, ZP/WCS shape. State = `published` if `!requireApproval || trusted_publisher || has data.moderate`, else `pending`. Records one `publication` event. |
| `POST /projects/{id}/frames/{uuid}/version` | publisher | new content version (re-calibration). Sets `supersededBy` on the old. |
| `PATCH /projects/{id}/frames/{uuid}` | coordinator / `data.moderate` | `accepted`, `acceptedReason`; `filterCanonical` (remap) — bumps `manifestVersion`, never a re-transfer. |
| `POST /projects/{id}/frames/{uuid}/approve` · `/reject` | coordinator / `data.moderate` | first-publication moderation; `approve` may also set `trusted_publisher = true` (the portal's default when approving). Reject is refused once any holder other than publisher/coordinator exists (R13). |
| `PUT /projects/{id}/holders/self` | device | body `{full: bool, add: [{uuid, contentVersion}], remove: [uuid]}`. Delta by default; the app sends `full=true` every 6 h and after any local rebuild. Replaces `report_have_set`. |
| `GET /projects/{id}/frames/{uuid}/holders` | member | fresh holders with relay URL only (rule S1 unchanged). |
| `GET/PUT /projects/{id}/dictionary` | `thresholds.edit` | filter dictionary, versioned. |
| `PUT /projects/{id}/members/{id}/trust` | `members.manage` | set/revoke `trusted_publisher`. |
| `PUT /projects/{id}/grid` | `project.edit` | canonical grid. |
| `GET/POST /projects/{id}/thresholds` | as today | `validate_rules` now checks `(metricKey, op, value kind)` against the shared registry (§6.3). |

Removed: everything under `/announcements`, `report_have`, `report_have_set`.

Coverage for the portal and Summary is a SQL aggregate over `project_frames`
(`accepted AND state = published`, grouped by `filterCanonical`, summing
`exptimeSec`, counting distinct publishers, fresh holders per frame → min/avg
redundancy). One materialised view refreshed on bump is enough at this scale.

### 4.3 Hub load model

78 members ≈ 120 devices. Poll: 120 / 15 s = 8 req/s against the in-process
cache, zero DB. A publication of 100 frames costs one batch write and one bump;
every device then does one delta manifest fetch (100 rows) and, for
processors, starts fetching. Holder deltas: one small PUT per device per
change. The Postgres pool of 5 is not the bottleneck; the previous four full
queries per project per device per pass are gone.

## 5. App

### 5.1 Folder model

- `Collaboration root` — the special scan root of kind `collaboration`, set in
  File Manager → Folders. **Required** before any receive or publish (the
  background worker refuses too — today it silently used the working dir).
- Layout: `<Collab>/<project-slug>/<publisher-slug>/<fileName>`; my own
  contributions land in `<Collab>/<project-slug>/<my-slug>/`.
- `<Collab>/.athenaeum/blobs/` — the collab blob store (iroh-blobs fs store),
  separate from the personal-sync store in `<working_dir>/blobs`. Same volume
  as the landing folders by construction, so `ExportMode::TryReference` is a
  rename and `ImportMode::TryReference` never copies.
- `validate_transfer_dir` applies to the Collaboration root exactly as to the
  transfer folders (absolute, `PathPolicy`, write probe) minus the "no scan-root
  overlap" clause — it *is* a scan root.

### 5.2 Publish (contributor)

Trigger: the "Publish N" button, or auto-publish (R16) after any of: scan
finished, analysis finished, plate-solve finished, filter mapping changed,
external-calibration attestation set, thresholds/dictionary version moved.
Coalesced through `ComputeQueue` (one job per project, re-armed if events
arrive while running).

Per frame that passes the gate and is `Not published` or `Update pending`:

1. **Obtain the calibrated artifact.** Attested-external set: the source file
   itself, seeded **in place** — steps 2 and 3 do not apply to it: no copy,
   no stamps, the local row's path is the original's path (amendment A1). Otherwise: calibrate on the fly from linked masters exactly as the
   calibrated-lights export does (`export::calibrated_generator`), including
   hot-pixel map and OSC handling per the project's `splitOsc` option (R21:
   with `splitOsc = false` the OSC frame ships as a CFA float FITS with
   `channel = "osc"`).
2. **Write once** into `<Collab>/<project>/<me>/<fileName>` as float32 FITS with
   `CALSTAT`, `ATH_CSRC` (frame uuid or `external`), `ATH_CVER`, `ATH_PRJ`,
   the canonical filter as `ATH_FILT`, the WCS from the solve, and `ATH_ZP`.
   `fileName` = `c_<stem>.fits` made unique within the publisher folder.
3. **Register** the local project frame row (5.4) with `origin = own`.
4. **Seed by reference**: import the file with `ImportMode::TryReference` under
   tag `project/<pid>/<uuid>/<ver>`.
5. **Announce** in batches of ≤ 500 (`POST /frames`), state comes back
   `published` or `pending`.
6. Report holders delta (`add`).

No `collab_pub` staging, no stamped copy, no push-seed to a designated
processor: the swarm starts from the publisher and the versioned poll makes
processors notice within 15 s. If the publisher goes offline before any
processor finished, the frame simply has one holder until it returns (Summary
shows "1 holder" in the redundancy column — the coordinator can see it).

Peak disk on the contributor: one calibrated frame at a time in the generator's
scratch (OS temp, as today); steady: raw + 1 calibrated copy per frame.

### 5.3 Receive (processor, coordinator)

Every pass (version moved, or the retry cadence):

1. Delta-fetch the manifest; upsert local rows.
2. **Need set** = `state = published ∧ accepted ∧ not mine ∧ not on disk ∧
   not locally-declined ∧ policy matches`. `locally_declined` is set only by
   the loss-guard answer "stop holding" (R17) and cleared by Restore. Policy (`collab_projects.policy`
   JSON): `{all | filters[], publishers[], maxFwhmArcsec?, minStars?, byteBudget?}`;
   `auto_replicate` stays the master switch. Preview: "N frames, M GB" from
   the manifest before anything moves.
3. Order: rarest-first (fewest fresh holders), then oldest.
4. Fetch per frame through the existing assignment loop (`sharing/iroh/assign.rs`
   — a child is a frame; unchanged): providers = fresh holders of that content
   version minus self, relay URL only.
5. On completion of **each frame**: `export(TryReference)` into
   `<Collab>/<project>/<publisher>/<fileName>` (rename on the same volume),
   verify BLAKE3 already done by the store, register the local row with
   `origin = replica`, keep the tag as the seed (the store now references the
   landed file). Peak disk = one in-flight frame per lane above steady 1×N.
6. Holder delta `add`.

`sync.max_concurrent_receives` still bounds lanes; a collab frame fetch takes
one permit per frame.

### 5.4 Local storage (app DB)

`project_frames_local (project_id, frame_uuid, content_version, origin own|replica,
landed_path UNIQUE (for an own frame it may lie outside the Collaboration root, A1), byte_size, xxh3, blake3, size_mtime_seen, on_disk bool,
locally_declined bool, manifest_json, updated_at)` replaces
`project_contributions` and `project_packages`. Analysis for Lights comes from
`manifest_json` (every member sees the whole project's metrics without
downloading).

### 5.5 Disk truth

On every replication pass (the minutes-cadence pass, never the 15 s version
poll) and before every `full=true` holder report, for every local row: `stat`. Missing → `on_disk = false`, seed tag
dropped, holder delta `remove`. Size or mtime drift → rehash (xxh3 then
BLAKE3); mismatch → treated as missing (a replaced file is a missing file).
This is the check the current model lacks (holders were reported from the DB,
so a deleted file made a phantom holder).

### 5.6 Scanner

The Collaboration root is walked as today, and **every** file under it goes
through the reconcile branch against `project_frames_local` — by path, then by
`(project, xxh3)` — whatever its header carries (amendment A1: a replica of an
externally calibrated original has no `ATH_PRJ`); outside the root a file with
`ATH_PRJ` is still diverted to the same branch — *known* no-op, *moved* repairs `landed_path`,
*duplicate* warns, *unknown* warns and is listed in Lights as "not part of the
project" (R18). No `files`/`frames` rows on any branch. The scanner never
creates a project frame; only publish (own) and receive (replica) do.

## 6. Gate, metadata, thresholds

### 6.1 Preconditions (layer 1)

`calibrated` (linked masters usable, or the set is attested external) ·
`analysis present` · `plate-solved` (WCS + pixel scale; replaces "unknown pixel
scale") · `centre within the project radius` · `filter mapped to the
dictionary`. Each failure is a sentence in the row's tooltip and a row action
where one exists (solve, map filter, attest).

### 6.2 Filter dictionary and mapping (R8)

- Hub: canonical entries with aliases and kind. Default dictionary at project
  creation: L, R, G, B, Ha, OIII, SII, plus "Unknown" is *not* an entry.
- App: `filter_mappings (instrume, filter_raw) → canonical` per account,
  proposed by a normaliser (case-fold, strip vendor words, `red|r → R`,
  `h-alpha|ha|h_alpha|hα → Ha`, `o3|oiii → OIII`, `s2|sii → SII`, `lum|l|clear → L`,
  `green|g → G`, `blue|b → B`), confirmed once in the publish flow (a modal
  listing the unmapped raw names with a Select each). Also applied to
  `INSTRUME` spelling (a display alias, no gate effect).
- Coordinator remap (`PATCH filterCanonical`) re-buckets stacking groups on
  the next plan; the file's `FILTER` header is never rewritten.

### 6.3 Threshold registry (shared, R20)

One registry, three copies that must agree, pinned by tests on each side:

| metricKey | unit | ops | value |
| ---- | ---- | ---- | ---- |
| `fwhm_arcsec` | ″ | lte, gte | number |
| `eccentricity` | — | lte, gte | number 0–1 |
| `stars_detected` | count | gte, lte | integer |
| `median_snr`, `snr_weight`, `frame_snr` | — (advanced, setup-dependent) | lte, gte | number |
| `zero_point` | mag | gte, lte | number (advanced) |
| `not_trailed` | — | reject_if | true |

App: `collab/gate.rs` keeps the match; unknown rules are still skipped with a
`warn!` (defence in depth) but the hub no longer stores them. Hub:
`validate_rules` rejects unknown keys, ops and value kinds with a message
naming the rule. Portal: `METRICS` constant in `portal/src/metrics.ts` drives
three `Select`s and a number input with the unit as suffix; the value field
keeps the raw string while editing and parses on blur (fixes `0.6`).

### 6.4 Zero point (R9)

Computed by analysis for any plate-solved LIGHT: match detected stars to the
solver catalog (Gaia G magnitudes, `mag` in the catalog record), aperture flux
in ADU/s, sigma-clipped median of `mag + 2.5·log10(flux/exptime)`, stored in
`frame_analysis.zero_point` with `zp_stars` and `zp_sigma`. Comparable within
one canonical filter only. Bandpass correction to the Gaia G system is not
attempted; ZP is used only for relative weighting within a filter, where the
common offset cancels.

Equivalent exposure: `t_eq = t × 10^(0.4·(ZP − ZP_ref))`, `ZP_ref` = the
group's median ZP. Ladder when ZP is missing: `t / N²` from `FOCALLEN`/`APTDIA`
(or focal length from the solve and pixel size); else raw seconds with the plan
warning "exposure equivalence unknown for N frames — grouped by seconds".

## 7. Stacking a project

- The plan gate for a project skips Masters and Calibrate because every
  project frame is calibrated by definition — decided from
  `project_frames_local`, never from a `CALSTAT` card (amendment A1); paths,
  WCS, filter and exposure come from the local row and its manifest, so the blockers `masters | links | masterFiles` never apply; new
  blocker `grid` when the canonical grid is unset.
- Groups: colour mode × canonical filter × exposure class (class width in
  stops, default ±1, `grouping.exposureMode = strict | equivalent | none`, also
  available on personal sets). Colour mode stays a key until the channel-split
  spec (R21) lands.
- Reference: the canonical grid is the reference geometry; the reference frame
  for star matching is still chosen per group (existing two-pass pick), but
  every frame registers onto the grid, so all processors produce comparable
  masters.
- Partial coverage: the register stage writes NaN outside the footprint; the
  banded integrator treats NaN as absent (per-pixel N), writes a coverage map
  as an artifact, and the output stage offers `cropToCoverage ≥ K`. This needs
  a verified change in `integration/` — an acceptance run on a two-scale,
  two-FOV synthetic set is part of the wave.
- Weights: existing measure/weights plus ZP-derived `t_eq` as the exposure
  term.
- OSC: shipped as CFA (R21). Until the channel-split spec lands, an OSC frame
  forms its own colour group (today's behaviour) and joins super-luminance
  nowhere.

## 8. UI

### 8.1 App — project page (R2)

- **Summary**: goal per canonical filter with shortfall (seconds, from the
  manifest), publishers with counts, redundancy (frames with 1 holder, with
  ≥ 3), online holders, pending moderation (moderators only), latest events.
- **My contribution**: the table of §3.3 over all linked frame sets, with the
  sticky banner "N new frames pass the gate · Publish N · Review", the
  auto-publish switch, "Link another frame set", bulk publish on selection,
  row actions (solve, map filter, attest set external, Ask to exclude). "Publish
  as project" and "Contribute to <project>" on the frame set page open this tab.
- **Lights**: every project frame from the manifest (all members) with
  publisher, camera, filter, exposure, date, FWHM, ecc, stars, ZP, accepted,
  holders, and per-device on-disk state; filters by any column; the
  replication policy editor with the byte preview; Download / Restore / Delete
  (Delete edits the policy first, R17); the "files not part of the project"
  panel (R18); Exclude with reason for moderators.
- **Stacking**, **Export**: the existing tabs bound to the project's frames;
  Export uses the R1 hierarchy `<title>/<publisher>/<camera>/<filter>/`.
- **History**: hub `project_events` + local landings.

### 8.2 App — elsewhere

- Frame set page: the **Project** block (link state, published/new/failing
  counts, Publish N new, Open project) and the **Project** column in the
  frames table (§3.3).
- Projects list and sidebar: unpublished-count badge per project.
- Notifications (discrete outcomes only): "N new frames pass the gate" /
  "Published N frames · M held back (reasons)" / "Replication paused: N frames
  missing (M GB)" — each with a link into the right tab.
- Settings → Transfers: `collab.loss_guard_fraction` (default 0.10),
  `collab.loss_guard_bytes` (default 10 GB), poll interval (fixed 15 s shown
  read-only), replication lanes (existing).

### 8.3 Portal

- Wave 0 (R20): threshold editor with `Select`s and units; parse-on-blur
  values; editor visible to `thresholds.edit`; Admin gains join policy and
  default data role; delegate capability checkboxes; `ROLE_LABEL` everywhere.
- Project page (P4 of the v2 backlog, amended): goals and coverage from the
  manifest aggregate for everyone; for members, the per-publisher frame list
  (name, filter, exposure, FWHM, holders); the swarm panel becomes per-frame
  redundancy (not per package); filter dictionary editor and canonical grid
  editor under Admin; trust switch per member under Members.

## 9. Moderation (R11–R13)

- `requireApproval = false`: everything that passes the gate is `published`.
- `requireApproval = true`: a publisher's frames are `pending` until the
  publisher is trusted. Approving the first publication sets trust by default
  (checkbox in the portal and the app). Pending frames are served only to
  moderators (unchanged), so R13 holds by construction.
- Exclusion: `accepted = false` + reason; stacking and export skip excluded
  frames; holders keep the file; the contributor sees "Excluded: <reason>".
- Trust revocation puts the publisher's *future* frames back to `pending`;
  nothing already published changes.

## 10. Edge cases and rulings

- **Deleted replica** → re-fetched while the policy wants it (R17).
  **Deleted own file** → `Published · not on disk`, not re-fetched, "Ask to
  exclude" (R17/R12). **Mass loss** (> guard) → scanner "moved" lookup by hash
  first, then replication paused with "restore / stop holding" (R17).
- **Content drift** → treated as missing (5.5).
- **Foreign file** → inert, listed (R18).
- **Re-analysis flips the gate** on a published frame → informational status
  only; thresholds are prospective.
- **Frame set merge** → the surviving set keeps the link; **split** → asks
  which side keeps it; frames follow their set.
- **Same object, second camera** → same set (sky clustering), new rows, the
  project hierarchy separates them by camera.
- **Publisher offline after publishing** → single-holder frames are visible in
  Summary; no push-seed.
- **Two devices of one account** → one publisher, two holder rows.
- **Account deletion** → R14.
- **Coordinator remap of a filter** → manifest edit, groups re-bucket, no
  transfer.

## 11. Migration and compatibility

Clean cut, no data migration (4.1). Both the hub and the app bump the collab
API version; an app older than this design gets `409 collab_api_outdated` from
every collab route and shows the existing "update required" state. The wire
between apps (iroh blob requests by hash) is unchanged; `Msg` postcard indices
are untouched.

Dropped app tables: `project_contributions`, `project_packages`; dropped
folders: `collab_pub`, `collab_serve`, `collab_seed`, `collab_swarm`; the
personal-sync store stays where it is.

## 12. Risks and measurements

- **Relay share** (BRD Q2, still unmeasured): the first real project must
  report the fraction of bytes that went through relays (per device, in
  Summary → "Transport") so the relay fleet can be sized. Added to the
  telemetry rule (D4 T7).
- **Coverage integration** is a numeric change to `integration/`; it is judged
  on a full group at master level, with the §8 tolerance rows of the stacking
  spec, never on a probe.
- **Hub in-process cache** breaks with two hub replicas; the hub runs single
  instance today, and the cache invalidates on bump within the same process —
  noted, not solved.

## 13. Out of scope (their own specs)

OSC channel split and super luminance (R21); hub push over SSE (the versioned
poll leaves it optional); mDNS cross-account; frame range splitting (A11);
Perseus capture-agent publishing.

## 14. Waves

0. **Portal thresholds mini-cycle** (R20) — hub `validate_rules` registry,
   portal `Select`s, Admin door/role/caps. Independent, ships first.
1. **Hub model** — tables, routes, versions cache, dictionary, trust,
   coverage aggregate; hub tests; test-hub deploy.
2. **App exchange** — folder model, blob store under the Collaboration root,
   per-frame publish/receive by reference, holder deltas, disk truth, policy,
   loss guard, versioned poll. E2E on three instances (contributor, processor,
   coordinator) across the test relay; disk accounting asserted (steady 1×N on
   both sides).
3. **Gate and metadata** — external attestation, filter mapping flow, ZP in
   analysis, plate-solve precondition, registry shared with the hub.
4. **Project UI** — six tabs, frame set Project block/column, notifications,
   Settings → Transfers.
5. **Stacking a project** — skip calibrate, canonical grid, exposure classes,
   coverage integration with its acceptance run, project export hierarchy.
6. **Portal project page** — coverage, per-frame list, dictionary/grid
   editors, trust.

Each wave: its own plan under `docs/superpowers/plans/`, reviewed per task and
per branch, merged to local main, pushed and deployed only on the owner's
word.

## 15. Acceptance (whole design)

Three instances on the test hub + test relay, one project, `requireApproval`
on: the coordinator creates the project with a dictionary; contributor A links
a raw set with masters, maps `"Red"` → R, publishes 20 frames (pending) → the
coordinator approves with trust → processor B replicates with policy
`filters = [R]` and lands 20 files by reference (blob dir does not grow by the
payload); A shoots 10 more, auto-publish announces them within 15 s of the
scan, B fetches them; A deletes 3 own files (status only), B deletes 3 replicas
(re-fetched), B deletes 15 at once (paused, prompt); the coordinator excludes
one frame with a reason → B's stacking plan shows 29 frames; contributor C
attests an externally calibrated XISF set and publishes; B stacks the project
onto the canonical grid with two pixel scales and reads the coverage map; the
project WBPP export lands `<title>/<publisher>/<camera>/<filter>/`.

## 16. Amendments

- **A1 (2026-09-24, owner) — the path of a project frame lives only in
  `project_frames_local`.**
  - **Own frames can live anywhere.** An own frame may live outside the
    Collaboration root. The externally calibrated original (R3) is seeded in
    place by reference, with no copy and no stamps. Reference import works
    across volumes; only a receiver's landing needs the collab store's
    volume.
  - **Consumers read the table.** Disk truth, holder reports, seeding,
    stacking (§7), the project export and the scanner (§5.6) take paths
    from the table, never from the folder layout or a header card.
  - **Scanner.** The scanner never creates catalog rows under the
    Collaboration root, and reconciles every file there against the table.
  - **Calibration and metadata.** "Is calibrated" for a project frame is
    implied by its row. Frame metadata comes from the manifest.
  - **Plan ruling.** The wave-2 plan records it as P26.
- **A2 (2026-09-24) — the collab blob store is served on its own ALPN**
  (`athenaeum/collab-blobs/1`), so §11's "the wire between apps is
  unchanged" reads "`Msg` is unchanged; one ALPN is added".
  - **Why.** iroh-blobs 0.103 serves one store per protocol handler.
    Landing by rename needs the store on the Collaboration root's volume:
    a cross-volume rename fails outright on Windows (os error 17 is not the
    `EXDEV` 18 that iroh falls back on).
  - **Reference.** Wave-2 plan P1 and its reuse audit.
- **A3 (2026-09-24) — holder reports.** A full report is sent on every
  replication pass (20 min), not every 6 h (§4.2), because the hub counts a
  holder fresh for 75 min.
- **A4 (2026-09-25, owner) — live exchange replaces the wave-2 timing
  model; waves renumbered.**
  - **Supersedes.** `2026-09-25-collab-v3-live-exchange-design.md` replaces:
    - R17's mass-loss pause (now one non-blocking, reversible choice above 10
      deletions in a rolling 5-minute window, each settled for 60 s);
    - R19's versioned polling (now a hub event channel with live presence);
    - §4.2's `GET /me/project-versions` and per-frame holders routes;
    - §5.3 steps 3–6 and §5.5;
    - amendment A3.
  - **Scope.** It brings "hub push over SSE" (§13) into scope. Its rulings
    are L1–L13 and its invariants I1–I11.
  - **New wave numbering.**
    - 3 = live exchange;
    - 4 = gate and metadata;
    - 5 = project UI;
    - 6 = stacking a project;
    - 7 = portal project page.
- **A5 (2026-09-26, owner) — replicas outside the current Collaboration
  root count as gone.**
  - **Rule.** After the Collaboration folder is re-designated, a replica
    whose landed path lies outside the CURRENT root is no longer held. The
    storage engine feeds it into the deletion path of the live-exchange
    spec (L4): above 10 frames in the rolling 5-minute window it raises the
    one non-blocking, reversible choice; otherwise the frames are fetched
    again into the new root.
  - **Scope.** It rules held replicas. Idle rows are ruled at the next
    sweep once re-included; quarantined rows and rows awaiting the user's
    choice keep their old paths until the user decides. Own frames never
    count: they may live anywhere (A1).
  - **Wording.** A frame that no other member holds gets a "lost
    everywhere" notice that says the file is still in the previous
    Collaboration folder (with its path), never "restore it from the
    Trash".
  - **Runtime.** A mount change of the collab store restarts the live
    runtime on the new root; it waits until the new folder's marker is
    recorded.
- **A6 (2026-09-27, owner) — one publishing device per account per
  project.**
  - **Why.** The publisher is the account (§3), and a publisher's frames
    share one folder `<Collab>/<project>/<publisher>/`. File names were made
    unique only within the publishing device's own folder, so two devices of
    one account publishing into one project could announce the same
    `fileName` (e.g. `c_Light_0001.fits` from two rigs). Receivers would then
    rename locally and the names would differ between members, which defeats
    R4 (names are there to find files by eye, the same everywhere).
  - **Binding.** The hub keeps, per (project, account), the one device that
    may announce NEW frames. The first device that publishes into the
    project becomes it. An announce from any other device of that account
    is refused (409, naming the bound device). Membership stays per account.
  - **Other devices of the account** are ordinary exchange participants: they
    receive the project's frames under their own replication policy and hold
    and serve them as replicas, which adds redundancy. A frame is "own" on a
    device only when that device published it: the manifest row carries
    `publisherDeviceId` (already stored by the hub as `publisher_device_id`),
    and the app derives `own` from it instead of from the account. This
    corrects the earlier derivation by account, under which a second device
    of the account never fetched its account's frames (contrary to §10
    "Two devices of one account → one publisher, two holder rows").
  - **Data role still rules receiving.** A member whose data role is `send`
    stores nothing on any of its devices, so a second device of a `send`
    account does not receive the account's own frames either; only a
    `send_receive` account's other devices replicate them.
  - **Device replace keeps authorship.** A device that replaced another one
    of the same account (§9.5 of the live-exchange spec) treats the replaced
    device's frames as its own: it can post their new versions, and the hub
    then records it as their publishing device.
  - **Switching.** "Publish from this device" is an explicit, confirmed user
    action that moves the binding. After a switch, the previously bound
    device can still post new VERSIONS of the frames it published (only it
    has their raw sources), but no new frames.
  - **Names after a switch.** The publishing device makes a new `fileName`
    unique against every name of its publisher in the project manifest, not
    only against its local folder, so an earlier device's names are never
    reused. No hub-side uniqueness constraint is needed while the binding
    rules out two concurrent publishers.
  - **Trade-off, accepted by the owner.** An owner with two rigs on two
    computers cannot publish into one project from both at once: they
    switch the publishing device, or bring the frames to one machine.
