# Collab project page and exchange observability — design

**Date:** 2026-09-29 · **Status:** approved in dialogue, awaiting written-spec review
**Builds on:** `2026-09-23-collab-v3-per-frame-model-design.md` (§8.1 R2, the six-tab page that was never built),
`2026-09-25-collab-v3-live-exchange-design.md`, `2026-09-28-collab-v3-contributor-path-design.md`.
**Visual reference (approved):** `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
— the layouts, groupings, columns and copy in this spec follow it. Where the two disagree, this spec wins
(the one known difference is §5.4 "Last seen").

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
| D6 | Stacking and Export are not project tabs in this cycle. The portal is not touched. The wire is not touched. |

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
| Library | `send_receive` or coordinator (same rule as today's Receive) | Other members' published frames and their state on this device. |
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
  fwhmArcsec?, eccentricity?, starsDetected?, medianSnr?, zeroPoint?,
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
`medianSnr`, `zeroPoint`, `publisherAccountId`.

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
  publishedFrames, secondsByFilter: { <canonical>: i64 },
  qualityByCamera: [{ camera, filter, frames, medianFwhm, medianEcc }],
  holdsFrames, holdsBytes, holdsShare,   // share of all published frames
}
```

- Published counts and seconds come from the local manifest mirror (published, accepted rows).
- Holds = holder claims on the frame's current content version, mapped to the account through its devices,
  joined to `project_frames_local` on `frame_seq`.
- **No "Last seen" column** (differs from the mockup): only "connected now" is known (presence book); a
  last-seen time is not stored anywhere. Online/offline dots only. Last-seen is a follow-up item.
- The Members table sorts on every column; a row expands to cameras and per-filter median FWHM/ecc.

### 5.5 Overview goals

The hub stores `goals` as free-form JSON and returns it with the project; the app does not read it today and
the portal has no goal editor. The app parses `goals` when it has the v3 spec §3.1 shape
`{ "<canonical filter>": <seconds> }` and draws a goal marker and "N to go"; otherwise the bars show accumulated
integration only. A goal editor on the portal is out of scope (open item).

## 6. The exchange meter

### 6.1 Shape

One `ExchangeMeter` per process, owned by the `collab_live` runtime. Keyed by
`(project_id, peer_device, direction)`; each entry holds bytes this session, frames in flight
(`frame_uuid`, file name, size, bytes done), completed count, an EMA rate over a ~10 s window, `started_at`,
`last_moved_at`. Writes are synchronous (atomics plus a short `std::sync::Mutex`, never held across an
`await`), so the runtime rule "the loop never awaits" holds. An entry idle for 60 s is dropped; its bytes are
already in the session history (§7) for receives.

### 6.2 Feeds

- **Receive.** `assign.rs`'s live child loop already consumes each provider's `Progress(u64)` stream itself
  (not the stock downloader's buffered channel, so the "telemetry is a sample" caveat on
  `ProviderTelemetrySink` does not apply to this path). `ProviderEvent` gains
  `Delivered { provider, item, bytes }`, emitted from that loop as deltas. The executor passes a sink that
  writes the meter instead of `noop_provider_telemetry()`. At landing the executor keeps the
  `AssignmentReport` for §7.
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

- `get_collab_exchange { projectId? }` → `ExchangeSnapshot { projects: [ProjectExchange] }`;
  `ProjectExchange { projectId, title, recv: [PeerFlow], send: [PeerFlow], toGo, waitingForPublisher }`;
  `PeerFlow { device, deviceName, memberName?, bytesSession, rateBps, etaSecs?, completed, queued,
  inFlight: [{ frameUuid, fileName, size, done }] }`. No `projectId` = every project (Transfers).
- Event `collab-exchange-progress`: the same shape, changed projects only, at most once per second, only while
  something moves, then exactly one all-zero event per project when it goes quiet. Throttling follows
  `FanOutTicker`.
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
  frames INTEGER NOT NULL, bytes INTEGER NOT NULL, retried INTEGER NOT NULL DEFAULT 0,
  sources_json TEXT NOT NULL DEFAULT '{}'   -- { device: bytes }
)
```

- A landing extends the project's newest session when its `finished_at` is ≤ 5 min ago, else opens one.
  Written inside the existing landing transaction (`BEGIN IMMEDIATE`, the collab read-then-write rule).
- `sources_json` sums the landing's `AssignmentReport.per_provider` bytes by device.
- `retried` counts frames whose fetch switched provider after a failure (today such failures are not recorded).
- Keep the newest 500 sessions per project; prune on insert.
- Command `list_collab_receive_sessions { projectId?, limit }` on both hosts.

### 7.2 Per-frame provenance

The landing's `sync_history` row keeps being written, with `peer_device` = the provider that delivered the
most bytes instead of `"swarm"`. The drawer reads it as "received <date> from <member>".
`api::sync::list_history` excludes collab landings (`project IS NOT NULL AND package_id IS NULL`) so they no
longer crowd out personal transfers; sessions replace them in the Transfers history.

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
| `get_collab_member_summary` | `get_collab_project_detail` (parsed goals, §5.5) |
| `get_collab_exchange` | |
| `list_collab_receive_sessions` | |

New event: `collab-exchange-progress`. Schema: `collab_receive_sessions` and `project_frames_local.announced_at`
(§5.1). The CLAUDE.md command count and module list are updated in the same change.

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

**Owner smoke (three instances on the test hub):** A publishes, B receives from A and C; both ends show
live rows with names, rate and ETA; B's Transfers shows the project group and a session in history with both
sources; A's Published shows B as holder in the drawer; Members holds match.

## 11. Delivery

Two waves, one plan each:

1. **Core and commands**: meter + feeds, snapshot/event, own-frames/holders/member-summary commands,
   `ProjectFrameView` fields, sessions + provenance + history filter, goals parsing; both hosts; tests.
2. **Frontend**: `ProjectFrameTable` + pure modules, the six tabs, drawer, Exchange tab, Transfers groups and
   history, sidebar indicator; tests.

## 12. Open items (not this cycle)

- Portal goal editor with the `{ filter: seconds }` shape (§5.5).
- Member "last seen" (needs the hub or presence to persist it).
- Send journal, if D2 is ever revisited.
- Relay/direct per peer.
- Project Stacking and Export tabs (v3 §8.1).
- Night of another member's frame using the publisher's longitude.
