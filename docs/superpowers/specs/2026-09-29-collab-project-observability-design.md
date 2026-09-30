# Collab project page and exchange observability — design

**Date:** 2026-09-29 · **Status:** approved in dialogue (hub-first amendment included), awaiting written-spec review
**Builds on:** `2026-09-23-collab-v3-per-frame-model-design.md` (§8.1 R2, the six-tab page that was never built),
`2026-09-25-collab-v3-live-exchange-design.md`, `2026-09-28-collab-v3-contributor-path-design.md`.
**Visual reference (approved):** `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
— the layouts, groupings, columns and copy in this spec follow it. Where the two disagree, this spec wins
(no known differences).

## 1. Problem

The owner's smoke of the collab exchange found two things that do not read as a finished product.

1. **The exchange is invisible.** During a receive, Transfers shows only landed files: no source, no time,
   no rate. The sending side shows nothing at all. The cause is in the core, not the UI:
   - receive: the executor knows which frames are in flight, and `run_live` returns per-provider bytes, but the
     report is discarded (`api/collab_live/executor.rs:897`) and the telemetry sink is a no-op (`:879`);
   - every landing writes one `sync_history` row with `peer_device = "swarm"` (`landing.rs:753`);
   - serve: the collab provider-event loop drains the update stream and records nothing
     (`sharing/iroh/mod.rs:1213`);
   - no command exposes holder devices, names or presence — only per-frame `holdersOnline/holdersTotal`.
2. **The project page is a pile of lists.** The Contribute tab prints every candidate frame in `GateTable`, then
   every published frame again as a card in `PublicationHistory`; Receive is a third list. Nothing is a real
   table, nothing sorts or filters, and there is no summary of progress, of who holds what, or of what is
   missing.

## 2. Decisions (owner, 2026-09-29)

| # | Decision |
| ---- | ---- |
| D1 | Exchange observability lives in **both** places from **one core source**: Transfers shows everything flowing through the device (projects as grouped rows), the project page has an Exchange tab for that project. |
| D2 | "What I gave to others" is answered from **holder data** (who holds my frame now), not from a send journal. Serving is visible **live**; it is not journaled. |
| D3 | The project page has six tabs: **Overview · My frames · Library · Members · Exchange · Moderation**. My frames is one tab with three segments — **Ready · Published · Held back** — because it is one pipeline. |
| D4 | Every frame list is a **grouped table with drill-down**: each tab has its own default grouping, aggregates sit under their own columns, a flat list is "Group by: None". Facet filters (filter, camera, night, publisher, state, name search), column sort, column visibility, selection with `(N of M)` actions. |
| D5 | Live telemetry is an in-memory **exchange meter** in the core, read by a snapshot command plus a throttled event (approach 1). The frontend does not aggregate per-frame events. |
| D6 | Stacking and Export are not project tabs in this cycle. The iroh wire is not touched. |
| D7 | **Hub first.** Per-filter goals become a validated, editable hub field (portal Admin editor), and member last-seen is exposed by the hub, before the app work starts (§5.6, wave 0). |
| D8 | Last seen is visible to **current members of the same project** only; anonymous and non-member viewers get `null`. |

## 3. Who reads the tables, and for what

This is the reasoning behind D3/D4; every default below is derived from it.

| Persona | Questions | Unit of thought |
| ---- | ---- | ---- |
| Contributor (most of the 78) | Did last night's Ha pass? Why are 40 frames held back, what fixes them? Did my frames arrive and count? How many hours per filter did I give? | night × filter |
| Processor | Do I have the complete dataset, what is missing and why? Whose data is better, what goes in the stack? How much disk? | publisher × camera × filter (a stacking group) |
| Coordinator | Which filters lag the goal? Who contributed how much, at what quality? What exists in one copy only? What should be excluded? | member × filter |

Nobody reads thousands of frames one by one; people think in groups and drop to frames for outliers.

## 4. Page structure

The header keeps what exists today (title, target, coordinator chip, `CollabLiveStatus`, Manage on portal,
publishing-device line, `UpdateRequired`, the auto-publish and auto-replicate switches). Tabs, in order:

| Tab | Visible to | Content |
| ---- | ---- | ---- |
| Overview | all | Integration per canonical filter as bars coloured by member (goal marker when goals exist, §5.5); my three numbers (ready / published / held back), each opening its segment; Needs attention; Exchange now (live one-liner); quality thresholds (moved from today's overview). |
| My frames | all | Segments Ready / Published / Held back with counts, `+ Link an object`, `Recalibrate and republish all`; one `ProjectFrameTable` per segment. |
| Library | `send_receive` or coordinator (same rule as today's Receive) | Every published project frame — mine and other members' — and its state on this device (§16). |
| Members | all | Table of people (§5.4). |
| Exchange | all | Live per-member rows both directions, then receive history (§7). A contributor sees only the Sending block and a line saying their role does not receive. |
| Moderation | coordinator, when `requireApproval` | Pending first publications. |

Tab badges: My frames = ready count; Library = frames still to come (downloading + queued + missing, not
"not kept"); Moderation = pending count (as today). Each frame lives in exactly one tab and one segment.

### 4.1 `ProjectFrameTable` — the one table

One generic component, parameterised per use by a column set, grouping options and actions. Layout top to
bottom:

1. **Facet row**: filter chips (canonical names, counts), Camera select, Night select (All · Last night ·
   Last 7 nights · Last 30 nights), Publisher select (Library, Moderation), a per-tab state select, name search,
   `Clear filters`. Every count is computed against all *other* active facets.
2. **Group row**: `Group by [level 1] ▸ [level 2]` (level 2 may be None; level 1 None = flat list),
   `Expand all` / `Collapse all`, `Columns ⚙` (visibility, per tab, remembered).
3. **Totals strip** over the *filtered* set: frames shown of total, Σ integration, size, median FWHM,
   selection count with clear, then the tab's actions.
4. **The table**: sticky header, 28 px rows (29 with border), right-aligned tabular numbers, units in the
   header, windowed rendering. Group rows carry a tri-state checkbox (selects its frames), caret, label,
   frame count, and **aggregates under their own columns**: Σ exposure under `Exp / Σ`, `x̃` medians under
   metric columns, sums under Size, a status breakdown under Status, a have/downloading/queued/missing/not-kept
   bar under "On this device", "N single" under Holders.
5. **Frame drawer** on row click (Escape closes): identity, metrics, gate rule-by-rule (value · needs · ✓/✕)
   for own frames, **who holds it** (member, device, online dot, publisher chip), version, local path, and for
   received frames "received <date> from <member>" (§7.2).

Sorting: a column click sorts frames within groups; groups sort by that column's aggregate when it has one,
otherwise by their natural order (night newest first, filters L R G B Ha OIII SII OSC, reasons by
`BLOCKER_ORDER`, names alphabetical). Ties break on file name. Default sort: name ascending.

Actions act on the eligible part of the selection and say so: `Publish 34 of 50`. With no selection the
primary action covers the whole filtered view (`Publish all 214`, `Approve all 57`). Buttons with zero eligible
frames are disabled, never hidden.

State: facets, grouping, sort and expansion per table via `useSessionState`; column visibility in
`localStorage` (wrapped in try/catch, per the storage rule). The first render of a grouping expands the first
group and its first child.

### 4.2 Per-tab configuration

| Table | Default grouping | Default columns | Other columns | State facet | Actions |
| ---- | ---- | ---- | ---- | ---- | ---- |
| Ready | Night ▸ Filter | Frame, Filter, Camera, Exp, FWHM, Ecc, Stars, Size | Night, SNR, ZP | — | Publish (primary) |
| Held back | **Reason** ▸ Night | Frame, Why held back, Night, Filter, Exp, FWHM, Ecc, Stars | Camera, SNR, Size | Reason | Solve / Analyze on selection; per-group fix button on Reason headers |
| Published | Night ▸ Filter | Frame, Filter, Exp, Ver, Status, Holders, On disk, Published, Size | Night, Camera, FWHM, Ecc | Status (+ "Only one copy", "Not on disk / changed") | Ask to exclude, Republish |
| Library | Publisher ▸ Filter | Frame, Publisher, Night, Filter, Camera, Exp, FWHM, Ecc, Holders, On this device, Size | Stars, SNR, ZP | On this device | Download, Stop keeping, Exclude (coordinator) |
| Moderation | Publisher ▸ Night | Frame, Publisher, Night, Filter, Camera, Exp, FWHM, Ecc, Stars, SNR | ZP, Size | — | Approve (primary), Reject |

Grouping options: Ready — Night, Filter, Camera, Object, None; Held back — Reason, Night, Filter, Camera,
Object, None; Published — Night, Filter, Status, Camera, None; Library — Publisher, Filter, Camera, Night,
None; Moderation — Publisher, Night, Filter, Camera, None.

Held back specifics: a Reason group header carries the existing `GateBlockers` action for that cause
(`Solve 44`, `Analyze 61`, `Map filter`, `Open calibration`, build masters, attest); a threshold cause has no
button and reads "quality — the frames themselves". A frame row shows its first failure as a chip
(`FWHM 3.42″ > 3.00″`) plus `+N` when there are more.

Library specifics: "missing" always carries its reason — `holder offline` when `holdersOnline == 0` and the
publisher has published it, `publisher offline` when `waitingForPublisher`. A downloading frame shows a
progress bar and percent from the exchange snapshot (§6.4).

Existing actions keep their current commands (publish, republish, solve, analyze, filter mapping, calibration
link, attest, approve/reject, exclude, keep/stop keeping). This cycle moves them; it does not change them.

## 5. Data for the tables

### 5.1 My frames — new command `list_project_own_frames { projectId }`

One row per LIGHT frame of the linked sets, joining the gate verdict, the contributor state and, when
announced, the local own row. It exists because `list_collab_frames` sees manifest frames only.

```
OwnFrameRow {
  frameId, frameUuid?, fileName, setId, setName?,
  night,                // catalog night (noon-to-noon), YYYY-MM-DD
  filter, filterMapped, // canonical name when mapped, else the raw FILTER with filterMapped = false
  camera?, exptimeSec?, byteSize,
  fwhmArcsec?, eccentricity?, starsDetected?, medianSnr?,   // no zeroPoint — amended, see §13.3
  segment,              // "ready" | "published" | "held"
  contributorState,     // ContributorState::key()
  failures: [{ kind, text }],   // kind ∈ BLOCKER_ORDER keys; sorted by that order
  // announced frames only:
  contentVersion?, pubState?, acceptedReason?, holdersOnline?, holdersTotal?, localState?, publishedAt?, lastError?
}
```

- `segment` derives from `collab::contributor_state::derive` and nothing else: `NotPublished` → `ready`,
  `FailsGate` → `held`, every other state → `published`. No second derivation.
- `failures[].kind`: `collab::gate::derive_blockers` today classifies rows into causes; it gains a per-row
  variant (one function, used by both) so each frame knows its kinds. The first kind is the Reason group.
- `publishedAt` = new column `project_frames_local.announced_at` (idempotent `ALTER TABLE … ADD COLUMN`; no
  announce time is stored locally today), set when this device's announce of the row succeeds. Existing own
  rows are back-filled once from `state_changed_at` — approximate, and labelled nowhere as exact.
- `evaluate_collab_gate` stays: it still feeds Held back's group-header actions and the frame set's Project
  block.

### 5.2 Library — extend `ProjectFrameView`

New fields, read from `project_frames_local.manifest_json`: `camera` (`instrume`), `telescope`, `night`,
`medianSnr`, `publisherAccountId`. No `zeroPoint` (amended, see §13.3).

- `night` for another member's frame = the UTC date of `dateObs − 12 h`. We do not know the publisher's
  longitude; for Europe/Russia this equals the local night, for the Americas it can differ. Computed in the
  core so the frontend does no date maths.
- No `INSTRUME` alias handling (still unbuilt per the v3 spec A7 note); camera is the header string.

### 5.3 Holders — new command `get_collab_frame_holders { projectId, frameUuid }`

```
FrameHolderView { memberName?, deviceName, deviceShort, online, isPublisher, contentVersion }
```

Device → member through `members_json[].nodes` (`collab::snapshot::member_node_ids`); device name from
`collab_holder_devices.display_name`; online from the presence book. An unknown device is listed by its short
id with `memberName = null` — never dropped. Called only when the drawer opens.

### 5.4 Members — new command `get_collab_member_summary { projectId }`

```
MemberSummary {
  accountId, displayName, dataRole, coordinator,
  devices: [{ name, online }],
  publishedFrames, secondsByFilter: { <canonical>: f64 },   // seconds, amended: f64 not i64, see §13.5
  qualityByCamera: [{ camera, filter, frames, medianFwhm, medianEcc }],
  holdsFrames, holdsBytes, holdsShare,   // share of all published frames
}
```

`CameraQuality.camera` (amended, see §13.5) is `""` when a frame had no `INSTRUME`; the UI labels that row
"Unknown camera" rather than the app inventing a placeholder string.

- Published counts and seconds come from the local manifest mirror (published, accepted rows).
- Holds = holder claims on the frame's current content version, mapped to the account through its devices,
  joined to `project_frames_local` on `frame_seq`.
- `lastSeenAt` (member level): "now" when any of the member's devices is connected per the presence book;
  otherwise the hub's `lastSeenAt` from the project response (§5.6.2), shown as `YYYY-MM-DD HH:MM:SS` with a
  relative "3 days ago" beside it; `null` = never seen. The summary carries `online: bool` and
  `lastSeenAt: string?`.
- Wave 1 contract: `HubClient::project_page` must send the device token (today it calls
  `/projects/{id}` anonymously, and under D8 an anonymous caller always gets `lastSeenAt: null`), and the
  app's `ProjectWire` / `MemberWire` gain `goals` / `lastSeenAt`.
- Devices in the expanded row show online/offline only; per-device last-seen is not exposed (it would need a
  holders-snapshot change).
- The Members table sorts on every column (last seen included); a row expands to cameras and per-filter
  median FWHM/ecc.

### 5.5 Overview goals

After wave 0 (§5.6.1) the hub guarantees `goals` is `null` or `{ "<canonical filter>": <seconds> }` with every
key in the project's dictionary. The app reads it from the project response it already fetches, strictly (a
malformed value is logged at `warn` and treated as `null`, never guessed at). A filter with a goal draws the
goal marker and "N to go" / "goal met"; a filter without one shows accumulated integration only; a goal for a
filter with no frames yet still gets its (empty) bar so the shortfall is visible.

### 5.6 Hub and portal (wave 0, repo `athenaeum-hub`)

Branch `project-observability-hub`, cut from `contributor-path-hub` (unmerged, awaiting the owner's §13
smoke; the app on `main` already targets it).

What exists: the portal types `goals` as `Record<canonical, seconds> | null`, the public project page's
coverage already carries `goalSeconds` per filter, and `project.edit`'s help text names goals. What is missing:
write-side validation (today only a 4 KB size cap in `validate_project_texts`) and any editor. The hub stores
`devices.last_seen_at`, stamped at most once a minute by both auth middlewares, but only the operator console
reads it.

#### 5.6.1 Goals

- **Validation** in `create_project` and `update_project`: `goals` is `null` or an object whose keys are
  canonical names in the project's filter dictionary and whose values are finite numbers with
  `0 < seconds ≤ 36 000 000` (10 000 h). Refusals name the offending entry:
  `goal for "Hb": not in this project's filter dictionary`, `goal for "Ha" must be a positive number of
  seconds`.
  On PATCH, `goals: {}` clears them (stored `NULL`); an absent or `null` field keeps the stored value (the
  route's existing `COALESCE` semantics).
- **Migration**: an existing row whose `goals` does not have that shape is set to `NULL`, each one logged at
  `warn` with the project id. Prod v3 is not deployed; the test hub is the only affected data.
- **Dictionary changes**: `put_dictionary` removes goals for canonicals it drops, in the same transaction.
- **Portal editor**: Admin → Settings gains a "Goals" block — one row per dictionary canonical (kind shown as
  on the dictionary editor), an hours input, empty = no goal, parsed on blur like the wave-0 threshold editor;
  visible and writable with `project.edit` or as coordinator; saved through the existing project update.
  Setting goals at creation (NewProject) is not added.

#### 5.6.2 Last seen

- `MemberPublicView` gains `lastSeenAt: DateTime<Utc> | null` = `max(devices.last_seen_at)` over the
  member's non-revoked devices.
- Filled only when the viewer is a current member of this project (D8); otherwise `null` for every row.
- Portal: the members list shows it for member viewers (relative time, absolute on hover).
- No new stamping: the existing throttled stamp is the source.

#### 5.6.3 Hub tests

Goals: unknown filter, zero, negative, NaN/string, `{}` clears (stored NULL) while `null`/absent keeps them, a dictionary edit dropping a canonical drops
its goal, the migration nulls a malformed row and keeps a valid one. Last seen: a member sees co-members'
values, a non-member and an anonymous viewer get `null`, a revoked device is ignored, the max across two
devices is taken. Portal: the goals editor (render per dictionary canonical, blur parse, empty clears, refusal
shown) and the members-list line.

## 6. The exchange meter

### 6.1 Shape

One `ExchangeMeter` per process. Amended (wave 1, see §13.6): node-owned (`SharedIrohNode::exchange_meter()`),
created at bind rather than owned by the `collab_live` runtime, since the serve feed (§6.2) fires from
connections the node accepts independently of any running runtime. Keyed by `(project_id, peer_device,
direction)`; each entry holds bytes this session, frames in flight (`frame_uuid`, file name, size, bytes
done), completed count, an EMA rate over a ~10 s window, `started_at`, `last_moved_at`. Writes are synchronous
(atomics plus a short `std::sync::Mutex`, never held across an `await`), so the runtime rule "the loop never
awaits" holds. An entry idle for 60 s is dropped; its bytes are already in the session history (§7) for
receives.

The runtime does not poll the meter on a fixed timer: it wakes on a `flow_started` signal (a new flow, or one
resumed after a pause) and then keeps emitting while `needs_progress` is true (bytes in flight, or a flow
moved within the last `MOVING` window) — see §13.6. A flow quiet longer than `MOVING` re-seeds its rate on the
next delivery instead of reading a stale average across the gap. An in-flight item's reported `done` is
clamped to its known `size` (a hedge or a retry re-delivering past 100% must not read over it).

### 6.2 Feeds

- **Receive.** `assign.rs`'s live child loop already consumes each provider's `Progress(u64)` stream itself
  (not the stock downloader's buffered channel, so the "telemetry is a sample" caveat on
  `ProviderTelemetrySink` does not apply to this path). Amended (wave 1, see §13.1): not a `ProviderEvent`
  variant — a separate `DeliveredSink` byte callback threaded through `LiveRunOptions`/`AssignmentOptions`
  into `transfer_once`, emitting deltas as bytes arrive. At landing the executor keeps the `AssignmentReport`
  for §7; the meter's own sum can exceed `AssignmentReport::total_bytes` (hedge losers, cancelled primaries,
  yielded items) — the meter is the per-peer truth, never reconciled against the report.
- **Serve.** The collab provider-event loop replaces the bare drain with a match on
  `RequestUpdate::Progress / Completed / Aborted`, tracking per-blob `end_offset` the way the personal path's
  `UploadAccumulator` already does. Project and frame come from the `ServeRecord` found at admission; the peer
  from `ConnRegistry` (connection → node id). The drain-for-the-whole-transfer safety rule and the permit
  lifetime are unchanged.
- **Queue figures** ("N frames to go", "waiting for publisher: N") are read from the scheduler's need set at
  snapshot time, not stored.

### 6.3 Device → person

Same mapping as §5.3. An unknown device shows its short id; the row is never dropped.

### 6.4 Surface (both hosts)

- `get_collab_exchange { projectId? }` → `ExchangeSnapshot { projects: [ProjectExchange], names }` (amended,
  see §13.4). `ProjectExchange { projectId, recv: [PeerFlow], send: [PeerFlow], toGo, waitingForPublisher }` —
  no `title` (the caller already has it) and no per-peer `queued` (the queue is per project, `toGo`, read from
  the scheduler's want set and kept current by the executor, not stored per event). `PeerFlow { device,
  bytesSession, rateBps, etaSecs?, completed, inFlight: [{ frameUuid, fileName, size, done }] }` — device ids
  only; names are a separate top-level list, `names: [{ projectId, device, memberName?, deviceName? }]`,
  resolved once against the catalog in the command, never per event. `waitingForPublisher` is populated only
  in this command's answer (it needs a catalog read); it is `null` in every `collab-exchange-progress` event.
  No `projectId` = every project that currently has flows (not every project the account belongs to).
- Event `collab-exchange-progress`: the same per-project/per-peer shape minus `names` and
  `waitingForPublisher` (device ids only, no catalog read on the emit path) — changed projects only, at most
  once per second, only while something moves, then exactly one all-zero event per project when it goes
  quiet. Throttling follows `FanOutTicker`.
- Rate and ETA are computed in the core. Personal sync keeps its frontend-computed rate; this cycle does not
  touch it.
- A contributor's snapshot has an empty `recv` (role does not receive).

### 6.5 Not in this cycle

Send journal (D2); per-peer relay/direct split; any wire change.

## 7. History and Transfers

### 7.1 Receive sessions — new table `collab_receive_sessions`

```
collab_receive_sessions (
  id INTEGER PRIMARY KEY, project_id TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT NOT NULL,
  frames INTEGER NOT NULL, bytes INTEGER NOT NULL, failed INTEGER NOT NULL DEFAULT 0,
  sources_json TEXT NOT NULL DEFAULT '{}'   -- { device: bytes }
)
```

- A landing extends the project's newest session when its `finished_at` is ≤ 5 min ago, else opens one.
  Written inside the existing landing transaction (`BEGIN IMMEDIATE`, the collab read-then-write rule).
- `sources_json` sums the landing's `AssignmentReport.per_provider` bytes by device.
- Amended (wave 1, see §13.2): the column is `failed`, counting landings that failed outright — a provider
  switch mid-fetch is not observable per frame, so `retried` was dropped.
- Keep the newest 500 sessions per project; prune on insert.
- Command `list_collab_receive_sessions { projectId?, limit }` on both hosts.

### 7.2 Per-frame provenance

The landing's `sync_history` row keeps being written, with `peer_device` = the provider that delivered the
most bytes instead of `"swarm"` — amended (wave 1, see §13.7): concretely, the top source's base64 device id
(the same string as `SnapshotMember.nodes`), or `"local"` for content linked from disk with no fetch
(`link_identical`); personal-sync rows are unaffected and keep their hex node ids. The drawer reads it as
"received <date> from <member>". `api::sync::list_history` excludes collab landings (`project IS NOT NULL AND
package_id IS NULL`) so they no longer crowd out personal transfers; sessions replace them in the Transfers
history.

### 7.3 Transfers page

- **Active**: personal rows unchanged; below them one expandable group per project with traffic, containing
  the same per-member rows as the Exchange tab, fed by `get_collab_exchange` + `collab-exchange-progress`;
  `Open in project →` links to the Exchange tab.
- **History**: personal entries and receive sessions merged by time; a session reads
  "M31 Deep Field 2026 · 48 frames · from Kostya, Olga · 9.1 GB · 31 MB/s" with a project chip.
- **Sidebar `TransferIndicator`** includes collab traffic (combined rate, active state); today it is silent when
  only collab is moving.

### 7.4 Notifications

None added. Progress stays an event (the notification rule: discrete outcomes only; the existing collab
outcome notifications are unchanged).

## 8. Frontend removals and moves

- Removed: `GateTable` and `PublicationHistory` (in `ProjectDetail.tsx`), `ReceiveTab.tsx`, the four-tab switch.
- Moved: `GateBlockers` actions into Held back's Reason group headers; `ModerationQueue` becomes the Moderation
  table's actions; thresholds and members into Overview/Members.
- `ProjectDetail.tsx` (986 lines today) splits into a page shell plus one component per tab under
  `src/components/collab/project/`; table logic (grouping, aggregates, facet counts, windowing) lives in pure
  modules beside `ProjectFrameTable`.
- Tailwind design tokens only; no new dependencies (windowing is hand-written, fixed row height).
- New TS types generated through `ts_export.rs`.

## 9. Commands and events summary

Both hosts, each Tauri command with its Axum mirror (`#[tracing::instrument(skip_all, err)]`;
`get_collab_exchange` at `level = "debug"` because the UI may call it on every mount):

| New | Changed |
| ---- | ---- |
| `list_project_own_frames` | `list_collab_frames` (§5.2 fields) |
| `get_collab_frame_holders` | `list_sync_history` (excludes collab landings) |
| `get_collab_member_summary` | `get_collab_project_detail` (goals, member `lastSeenAt`; §5.5, §5.6) |
| `get_collab_exchange` | |
| `list_collab_receive_sessions` | |

New event: `collab-exchange-progress`. Schema: `collab_receive_sessions` and `project_frames_local.announced_at`
(§5.1). The CLAUDE.md command count and module list are updated in the same change.

Hub (wave 0): `create_project` / `update_project` validate `goals`; `put_dictionary` prunes goals; the project
response's members carry `lastSeenAt`; one migration normalises existing `goals`; portal Admin goals editor
and members-list last seen. No new hub endpoint.

## 10. Testing

**Rust (core):**
- Meter: EMA converges; idle entry dropped at 60 s; in-flight bookkeeping on complete/abort.
- `run_live` with two fake providers: Σ `Delivered` equals `AssignmentReport::total_bytes`.
- Serve: two in-memory nodes via the live-exchange harness; the serving side's snapshot names the peer and the
  bytes.
- Event throttle: ≤ 1 per second, exactly one final zero event.
- Own frames: `segment` for every `ContributorState`; `failures[].kind` in `BLOCKER_ORDER`.
- Member summary: two devices of one account aggregate, an unknown device survives, holds count only current
  versions.
- Night of another member's frame at the UTC-noon boundary.
- Sessions: open/extend/close on a fake clock (5-min edge), `sources_json` sums, `retried`, prune at 500;
  `list_history` excludes collab landings.
- Full `cargo test -p athenaeum-core` (all targets) before any push.

**Frontend (vitest):** grouping and aggregates, facet counts ignore their own facet, sort within and across
groups, `(N of M)` eligibility, windowing slice maths, Transfers history merge order, indicator on collab-only
traffic, role-dependent tabs.

**Owner smoke (three instances on the test hub):** the coordinator sets goals on the portal and the app's
Overview draws them; a member who quit an hour ago shows that time in Members; A publishes, B receives from A and C; both ends show
live rows with names, rate and ETA; B's Transfers shows the project group and a session in history with both
sources; A's Published shows B as holder in the drawer; Members holds match.

## 11. Delivery

Three waves, one plan each, in order:

0. **Hub and portal** (§5.6): goals validation, migration, dictionary pruning, Admin goals editor, member
   `lastSeenAt`; hub and portal tests. Deployed to the test hub before wave 1's owner smoke.
1. **Core and commands**: meter + feeds, snapshot/event, own-frames/holders/member-summary commands,
   `ProjectFrameView` fields, sessions + provenance + history filter, goals and `lastSeenAt` reading; both
   hosts; tests.
2. **Frontend**: `ProjectFrameTable` + pure modules, the six tabs, drawer, Exchange tab, Transfers groups and
   history, sidebar indicator; tests.

## 12. Open items (not this cycle)

- Goals at project creation (NewProject); per-device last seen.
- Send journal, if D2 is ever revisited.
- Relay/direct per peer.
- Project Stacking and Export tabs (v3 §8.1).
- Night of another member's frame using the publisher's longitude.

## 13. Amendments (wave 1, 2026-09-30)

Wave 1 (core and commands) implementation found seven places where the built shape differs from this design;
each is fixed inline above and listed here for the record.

1. **§6.2 receive feed.** Not a `ProviderEvent::Delivered` variant — `ProviderEvent` is `Copy` and the stream
   is sampled, and the item key the meter needs is not in scope where bytes are actually read. The feed is a
   separate `DeliveredSink` byte callback (`sharing/iroh/assign.rs`) threaded through
   `LiveRunOptions`/`AssignmentOptions` into `transfer_once`. Its sum can exceed the fetch report's
   `total_bytes` (hedge losers, cancelled primaries, yielded items) — **the meter is the per-peer truth; it is
   never reconciled against the report.**
2. **§7.1 sessions column.** `failed` (landings that failed outright), not `retried` — a provider switch
   mid-fetch is not observable per frame.
3. **§5.1 / §5.2 — no `zeroPoint`.** Dropped from `OwnFrameRow` and the `ProjectFrameView` extension: the app
   does not compute a zero point anywhere (v3 R9 is unbuilt); the mockup's ZP column is dropped in wave 2.
4. **§6.4 surface shape.**
   - The progress event carries device ids only; names come from `get_collab_exchange`'s own separate
     `names: [{ projectId, device, memberName?, deviceName? }]` list, resolved once against the catalog in the
     command, never per event.
   - `ProjectExchange.title` and per-peer `queued` are dropped — the queue is per project (`toGo`, from the
     scheduler's want set, kept current by the executor, never stored per event).
   - `get_collab_exchange` with no `projectId` returns the projects that currently have flows, not every
     project the account belongs to.
   - `waitingForPublisher` is populated only in the command's snapshot (it needs a catalog read); it is `null`
     in every `collab-exchange-progress` event.
5. **§5.4 member summary.** `secondsByFilter` values are `f64` seconds, not `i64`. `CameraQuality.camera` is
   `""` when a frame had no `INSTRUME` header (the UI labels that row "Unknown camera").
6. **§6.1 / §6.4 meter.** The meter is node-owned (`SharedIrohNode::exchange_meter()`), created at bind, not
   owned by the `collab_live` runtime — the serve feed fires from connections the node accepts independently
   of any running runtime. The runtime wakes on a `flow_started` signal and then keeps emitting while
   `needs_progress` is true, rather than polling on a fixed timer. A flow quiet longer than `MOVING` re-seeds
   its rate on resume instead of reading a stale average across the gap. An in-flight item's reported `done`
   is clamped to its known size.
7. **§7.2 provenance.** `sync_history.peer_device` for a collab landing is the top source's base64 device id
   (the same string as `SnapshotMember.nodes`), or `"local"` for content linked from disk with no fetch;
   personal-sync rows are unaffected and keep their hex node ids.

## 14. Amendments (wave 2, 2026-09-30)

Wave 2 (frontend) implementation found six places where the built shape differs from this design, and the
controller made five further rulings that change a documented contract; both are listed here for the record.

1. **§4.2 actions — publish and republish.** Publish and republish take an optional `frameIds`. The primary
   action always sends the ids it shows. Every republish goes through a guard dialog, which asks for the typed
   frame count for "all" or for more than 100 frames.
2. **§4.2 actions — Exclude / Restore.** Exclude/Restore (coordinator) are built on the hub's existing
   `PATCH …/frames/{uuid}`. Ask to exclude, Download and Stop keeping (held) are not built (hub mechanism /
   new core model).
3. **§4 page.** The project page no longer calls `evaluate_collab_gate`. Held back's Reason actions work from
   the group's rows.
4. **§4.1 drawer.** `FrameGateRow` / `OwnFrameRow` carry `rules[]` (`RuleVerdict`) from `evaluate_frame`, and
   `OwnFrameRow` carries `path` and `accepted`. The drawer is rule-by-rule, as §4.1 says.
5. **§5.2 `ProjectFrameView`.** Gained `receivedAt` / `receivedFromDevice` / `receivedFromMember`.
6. **§4.2 Published table.** "Only one copy" means `holdersTotal + (this device holds it)` = 1, because
   `holdersTotal` counts other devices only.
7. **§4.2 My frames — "Recalibrate and republish all".** Sends the guard's own target ids (own published,
   non-excluded `frameIds`), never `frameIds: null` — a null republish runs force over every gate candidate
   and would announce never-published Ready frames, contradicting "publish sends the ids it shows" and "no
   100 TB by one click". The typed-count rule still keys on "all" (controller ruling, Task 16).
8. **§4.2 Moderation — batch approve/reject.** An approve-with-trust cascade can answer a later frame of the
   same publisher with Conflict "This frame was already decided — refresh the queue." (detected by that
   stable core message prefix). The batch loop treats that one refusal as benign — the frame is no longer
   pending, so it counts as decided and the loop continues; any other error still stops it (controller ruling,
   Task 12).
9. **§6.4 / §8 `ExchangeState`.** Keeps a per-project `summary` (`toGo`, `waitingForPublisher`) separate from
   the live-flow map: the meter keeps a finished flow for 60 s reporting `moving: false, rateBps: 0`, so both
   `applySnapshot` and `applyProgress` filter that ghost out of `projects` — but `summary` is written for
   every project named in a snapshot's answer and is never deleted by a quiet event or by `clearFlows`; it is
   not flow data (controller ruling, Task 8).
10. **§4.2 Library — Exclude dialog reason.** The reason-length check counts Unicode characters
    (`[...reason.trim()].length`), matching the core/hub count, not UTF-16 code units (controller ruling,
    Task 9).
11. **§6.3 device → person.** `member_of_device` moved from `api::collab_live::surface` (render-gated) to
    `collab::snapshot` (ungated), required so the headless build still compiles it; behaviour-preserving
    (controller ruling, Task 2).

## 15. Amendments (smoke fixes, 2026-09-30)

The owner's first smoke of the wave-2 project page found three defects, fixed the same cycle. Plan
`docs/superpowers/plans/2026-09-30-collab-smoke-fixes-names-moderation-presence-plan.md`.

1. **§5.3 holder snapshot — device names.** The hub's holder-snapshot query sent the member's own display
   name for every one of their devices (`pm.display_name`, not `d.name`), so the app stored the member's name
   as every device's `displayName`. Fixed at the hub (`COALESCE(d.name, '')`, hub commit `b485578` on branch
   `holder-device-names`); the app needed no change — its existing empty-name-falls-back-to-short-id rule
   already covers a device with no name.
2. **§5.1 `ProjectCard` — `canModerate`.** Gained a `canModerate` field (`coordinator || data.moderate`, from
   the same cached `gov_caps_json` the card already carries). Moderation, Exclude and Restore, and the
   publish-confirm approval line, all gate on `canModerate` rather than `coordinator` alone —
   `needsApproval` is `requireApproval && !canModerate` (controller ruling: a `data.moderate` holder's own
   publish must never show a false "needs approval" line, matching the hub, which already authorizes
   decide/exclude/restore on `data.moderate`, not on `coordinator`).
3. **§4 Moderation tab.** Now two sections rather than one, visible to `canModerate`: "Waiting for review" —
   the existing pending queue, or the single line "This project publishes without review." when the project
   does not require approval; and "Excluded frames" — every frame with `accepted === false`, derived from
   the already-loaded library (no separate fetch), with a batch Restore action.
4. **New event `collab-peers-changed { projectId }`.** Raised by the collab-live runtime on presence, holder,
   membership (`FeedEffect::MembersChanged`) and epoch (`FeedEffect::EpochChanged`) changes — the brief scoped
   only holder/presence, but the owner's goal ("online, or the roster, shows without reopening the page")
   covers a member joining or leaving too (controller ruling, Task 2). Throttled to at most one per project
   per second (`PeerBurst`, the same `LANDED_BURST` window as the landed-frames burst). The project page
   re-reads at most one reload per 1 s window (library + members) and per 5 s window (own frames),
   scheduled by the first event; further events in the window are absorbed, never restarting it; an open
   frame drawer re-reads that frame's holders on the same 1 s-throttled event. No new
   command — the event goes through the existing emitter, and the web host forwards it over SSE unchanged.

## 16. Amendments (owner, 2026-09-30)

1. **Library = every project frame.** D3 had narrowed the v3 §8.1 "Lights" tab to other members' frames,
   which dropped the owner's own published frames out of the one list the project's WBPP export (already
   own and replica alike, `collect_project_export_data`) and a future project stack work from. The Library
   now lists every frame not pending moderation, mine included; an own frame's device column reads its own
   state (`own_held` → have, `own_missing` → missing / gone from disk, `own_changed` → changed). The badge
   still counts only frames to come, so own frames never count. Heading "Project frames".
2. **Other files.** The publish writes a frame's file before its own row exists, so the Collaboration-folder
   watcher could list it as foreign and nothing removed it: every published frame showed under "Other
   files". `record_own` now forgets the entry, and the list skips any path a frame row has landed at. The
   panel is one collapsed line with a count; opened, a bounded scrolling list of name + folder.
