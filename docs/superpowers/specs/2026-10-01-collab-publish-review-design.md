# Collab project page — reviewed publishing, live sync, Blink (review round)

**Status:** approved in dialogue 2026-10-01, written for review.
**Branch:** `collab-publish-review`, cut from `wave-5.5-project-ui` (unmerged, head `ffd55130`).
**Builds on:** `2026-09-29-collab-project-observability-design.md` (six-tab page, own-frame rows,
exchange meter) and `2026-09-30-collab-project-ui-pixel-design.md` (wave 5.5 look, `src/components/ui/`).
**Visual reference:** the canvas https://claude.ai/artifact/Eqn7HKZktqD3VfRpCqwE6z. A copy of its
artboards is in `docs/superpowers/research/2026-10-01-collab-publish-review-mockup/` (they render only
inside the canvas). Artboards: Overview, My frames, Blink (own To review), Blink (Library, moderator),
Blink (Library, member).

## 1. Problem and success criteria

After wave 5.5 the owner found five problems on the project page:

1. **Publishing happens without the member seeing it.** `collab_projects.auto_publish` defaults to `1`.
   After any of ~15 triggers (scan, analysis, plate solve, link, master build, calibration edits…),
   a run calibrates and publishes. It emits nothing until it ends. The bell then shows one line.
2. **A member cannot see what will be published.** My frames and the frame drawer show the raw
   source light (`OwnFrameRow.path`). The calibrated `c_<stem>.fits` exists only inside a publish
   run. There is also no way to say "do not publish this frame". A frame put in the Black Hole is
   still a candidate, and the next run publishes it.
3. **The meta line under the header is easy to miss.** "Auto-publish on/off" is small text that does
   not look like a button. It sits right under the title, and its explanation is only in a tooltip.
4. **Live does not refresh.** `collab_sync_now` queues a reconcile and returns at once. The page then
   re-reads only the card, before the reconcile has finished. "synced N ago" is the card's
   `fetchedAt`, which moves only when the project is re-fetched (a hub change or a manual Projects
   refresh), never when the hub confirms that nothing changed. My frames, Library and Members are not
   re-read at all. Manifest changes from the hub (`collab-frames-changed`) are not heard by the
   page. The member has to leave the page and come back.
5. **Frames cannot be inspected from the project.** Blink is not reachable from My frames or
   Library. Inside Blink, the only action is the Black Hole, which means nothing for a project.

The cycle is done when:

- A new frame reaches the hub only through Calibrate → Review → Publish, or through a mode the member
  chose explicitly (§4.6). Every new and every existing project starts in **Manual**.
- Every publish-family run, manual or automatic, shows its stage, `current / total` and current file
  on My frames while it runs. It can be cancelled there, and it leaves a one-line result.
- Blink opens from My frames and Library on the frames held locally. For own frames past calibration
  it shows the calibrated file that is (or will be) published. Its actions are the project's
  actions for the viewer's role, never the Black Hole.
- After clicking the Live pill, the timer restarts once the hub has confirmed the project. Every
  data set on the page is then re-read. A change pushed by the hub appears without leaving the
  page.
- The Overview has a **Project settings** card (publishing device, publishing mode,
  auto-replicate, each with visible help text). The meta line is gone. My contribution sits above
  Integration toward goal, and its tiles carry hours, nights, size and per-filter hours.

## 2. Owner rulings (2026-10-01)

| # | Ruling |
| ---- | ---- |
| P1 | **Default off for everyone.** Auto-publish no longer defaults to on. A one-time migration turns it off for every project already joined. New projects start in Manual. Release-note line. |
| P2 | **Publishing is three steps: Calibrate → Review → Publish.** Calibrate writes the calibrated files and records them as *prepared*, without seeding or announcing. Review happens in Blink on the calibrated files. Publish seeds and announces the prepared frames. |
| P3 | **Three modes per project** (local, never sent to the hub): **Manual** (everything by click), **Auto-calibrate** (triggers calibrate, frames wait in To review, Publish is a click), **Fully automatic** (calibrate and publish, no review, for an unattended rig). |
| P4 | **"Don't publish" is a local, reversible withhold.** A withheld frame is never calibrated or published by any run. If it was already calibrated, its calibrated file is deleted. **Release** returns it to Ready. A published frame cannot be withheld: a seeded frame is part of the project and is never withdrawn (owner rule); only a moderator excludes it. |
| P5 | **The Black Hole is not a project action.** Inside a project, Blink never offers it. A source light in the Black Hole is no longer a publish candidate. |
| P6 | **Updates of already-published frames skip review.** These are a recipe change (new master, raw edited) or a Republish. The frame was already reviewed, and Republish keeps its guard dialog. |
| P7 | **Blink by role.** Own unpublished frames get Don't publish / Release. Published frames get **Exclude from project** (reason required) for a moderator (`canModerate`). Everyone else views only. "Ask to exclude" stays out of scope (it needs the hub). |
| P8 | **"synced N ago" = the last time core confirmed the project against the hub.** This is the hub's `hello`, its 60 s `versions` beat, any applied project event, or Sync now. Sync now waits for that confirmation, then re-reads every data set on the page. |
| P9 | **Overview layout.** Left column: My contribution above Integration toward goal. Right column: Project settings, Needs attention, Exchange now, Quality thresholds. The header's second row (`MetaLine`) is removed. |
| P10 | **The canvas artifact above is the approved look** for the changed regions. Everything not drawn there keeps the wave 5.5 look. |

## 3. What the code does today (facts this design rests on)

- **Auto-publish.**
  - Column `collab_projects.auto_publish INTEGER NOT NULL DEFAULT 1` (`db/schema.rs`), written only by `db::collab::set_auto_publish`.
  - Worker `api/collab_autopublish.rs`: dirty sets plus a `KICK`, then a 30 s debounce, then `run_publish_pass` → `api::collab::auto_publish_collab_frames`.
- **One run per project.** `publish_lock` + `claim_publish` (`try_lock`). A second run is refused with `PUBLISH_BUSY_MSG`.
- **`run_publish` stages.**
  1. Gate.
  2. Hub credentials, node, store mount, device key, A6 binding.
  3. Split into New / Update / Adopt.
  4. Generation (`run_publish_generation`: one `ComputeQueue` slot, then `execute_generation` per frame).
  5. Seed by reference.
  6. Announce (batches of ≤500).
  7. Content versions.
  8. Own rows.
  9. `collab-published`. It is emitted at the end and before the outdated-hub / first-error returns
     (`collab.rs` ~5720-5745). It is **not** emitted on the early returns: signed out, store
     unmounted, A6 refusal.

  No progress events are emitted.
- **Attested sets** (`frames_set.calibrated_externally`) are never calibrated. The **original** catalog
  file is stat'd/hashed and seeded in place, with recipe = `external_recipe(size, mtime)`
  (`collab.rs` ~3261-3350, ~4310-4335).
- **Cancel exists only as the compute-queue X.** After that, every remaining frame is still opened and then held back as "calibration failed: cancelled", and already generated frames are still seeded and announced. A cancel while queued fails the whole run.
- **The calibrated light lives at** `<Collaboration root>/<project slug>/<member or "own">[_n]/c_<stem>.fits`.
  - It is written in place for a new frame; an update goes through `.athpub` + rename.
  - It is seeded by reference and never catalogued.
  - The own row (`project_frames_local`) records `landed_path`, `source_frame_id` and `recipe_hash`.
  - `delete_not_in` prunes **replica** rows only; own rows are never pruned (R12). A not-yet-announced
    frame still gets its own table (§4.2), because an own row feeds the New/Update split, `ensure_seeded`,
    the outbox claims and `taken_names`.
  - The own folder (`publisher_dir`, `collab_exchange.rs` ~1075-1129) is recomputed on every run until
    an own row exists.
  - Landing-name allocation (`new_frame_target`, `collab.rs` ~3048-3102) checks the DB, never the
    filesystem.
  - A file under the Collaboration root that no row names is recorded as foreign by the scanner
    (`reconcile_project_file`) and listed under "Other files" (`list_foreign_files`). Nothing deletes
    it.
- **Black Hole** only inserts a `black_hole` row. No collab code reads it.
- **Sync now** (`api/collab_live/runtime.rs::sync_now`):
  - `reset_all` back-offs, `reconnect_now`, `LiveCommand::Reconcile`, which queues `FeedWork::DigestAll` + `StorageWork::Sweep`. It returns before any of it runs.
  - `DigestAll` is a holder-claim digest (`PUT …/holders/self` per project, `holdings.rs` ~437-478), not
    a version check. Its results are discarded, and it lands on the serial feed worker **ahead of** the
    reconnect's `hello`.
  - The reconnect produces a `hello`. `FeedApplier::on_hello` catches up only the projects whose version moved.
  - The hub broadcasts `versions` every 60 s (`HEADS_EVERY`), handled by `FeedApplier::on_versions`.
    The first one after a connect comes 0–60 s after the hello.
  - `FeedApplier` holds an emitter but not the runtime's `Shared`; only `FeedWorker` does
    (`workers.rs`).
  - `on_hello` / `on_versions` log and skip per-project failures. A 403 there never becomes
    `FeedOut::Refused`.
  - `catch_up_project` discards the manifest change list and always reports `NeedSetChanged`.
- **Blink** (`BlinkViewer`, `src/components/blink/`):
  - It takes `FileWithFrame[]`. The Black Hole / Restore buttons are built in and cannot be hidden.
  - Images come from `read_fits_image_rustafits {path}` (desktop) or `get_frame_preview {frameId: file.id}` (web), as JPEG bytes.
  - Star metrics come from `get_frame_star_metrics {frameId}`.
  - Per-frame state (images, selection, metrics) is keyed by **array index** (`BlinkViewer.tsx`,
    `useBlinkCache.ts`). The list key is `file.id ?? index`.
  - A `window` keydown handler takes `s a 0 - + = Enter Space`, the arrows and Escape. Blink is not on
    the wave 5.5 overlay stack.
  - Library rows (`ProjectFrameView`) carry no catalog id and no path.

## 4. Publish model

### 4.1 States and segments

My frames has four segments. Each own frame is in exactly one:

| Segment (`OwnFrameRow.segment`) | Meaning | Table actions |
| ---- | ---- | ---- |
| `ready` | passes the gate; not prepared; not published; not withheld | **Calibrate**, Don't publish, Blink (raw) |
| `review` (new) | prepared: calibrated file written, recipe current, not announced | **Publish**, Don't publish, Blink (calibrated) |
| `published` | on the hub (incl. pending approval, update pending) | Republish, **Update** (update-pending only), Exclude (moderator), Blink (calibrated) |
| `held` | fails the gate, **or withheld by you** | Solve, Analyze, **Release** (withheld only), Blink (raw) |

**Contributor states.** `collab::contributor_state::ContributorState` gains two states:
- `Prepared`: segment `review`.
- `Withheld`: segment `held`, failure kind `withheld`.

`derive` is shared with the frame set's Project block (§11), so both read the same.

**Derivation order.** One function decides, `own_contributor_state` + `derive`, which gain the
prepared and withheld inputs. The first match wins:

1. **An own row exists** → today's own-row states (Published, PendingApproval, UpdatePending,
   Rejected, PublishedNotOnDisk, PublishedNowFailsGate). Withhold and the Black Hole never apply.
2. **Withheld** → `Withheld` (Held back, kind `withheld`, "Withheld by you").
3. **Source in the Black Hole** → `FailsGate` with kind `blackHole` ("In the Black Hole").
4. **The gate fails** → `FailsGate`. A prepared row, if any, is kept: `publish` re-gates, skips the
   frame and reports it held back. Withhold deletes the row; a later pass that clears the gate
   restores Review.
5. **Prepared**, with the recipe current and the file present at its recorded size → `Prepared`
   (To review). A prepared row that is stale → Ready; the next calibrate or publish drops the row.
6. Otherwise → `NotPublished` (Ready).

`current_recipe` must be computed for prepared rows too. Today it is computed only when an own row
exists (`collab.rs` ~1490).

**Touch list** (every exhaustive match or mirror of states, kinds and segments):
- Rust: `segment_of`, `ContributorState::key()` / `short_label()`, `ContributorCounts::bump`, `gate.rs::failure_kinds` (an unknown kind maps to `threshold` today).
- `derive_blockers`: `withheld` and `blackHole` are frame states, never set blockers.
- TS: `contributorState.ts` `SHORT`, `BLOCKER_ORDER` + its mirror in `table/model.ts`, `REASON_LABEL`, `attention.ts` `CAUSE` (else `console.error`), and in `ProjectDetail.tsx` the `Segment` / `AttentionTarget` / facet setter maps.
- The My frames tab badge becomes "N ready · M to review".

**Withheld rows.** In Held back they group under the reason "Withheld by you" through the existing
`failures[0].kind` grouping.

**`OwnFrameRow` gains:**
- `calibratedPath: string | null`: the prepared file, or the published `landed_path`.
- `calibratedBytes: number | null`.
- `preparedAt: string | null`.
- `withheld: boolean`.

`path` stays the raw source path.

### 4.2 Storage (local catalog, idempotent migrations in `db/schema.rs`)

- **`collab_projects.publish_mode`** `TEXT NOT NULL DEFAULT 'manual'`, one of `manual` | `auto_calibrate` | `automatic`.
  - The migration adds the column with the default and sets every existing row to `manual` (P1).
  - `auto_publish` stays in the table but is no longer read or written; SQLite has no cheap drop.
- **`collab_projects.last_publish_run`** `TEXT NULL`: JSON of the last finished run (§5.2 payload).
- **`collab_prepared_frames`**, primary key `(project_id, source_frame_id)`, holds:
  - `project_id`, `source_frame_id`;
  - `calibrated_path`: NULL for an attested frame; unique per `(project_id, calibrated_path)` when set;
  - `external INTEGER NOT NULL DEFAULT 0`: 1 for an attested frame, whose file is the catalog original;
  - `own_dir`: the own folder this frame landed in, pinned (§4.3);
  - `recipe_hash`, `byte_size`;
  - `prepared_at`, `publish_run_id`.
- **`collab_withheld_frames`**, primary key `(project_id, source_frame_id)`, holds `project_id`, `source_frame_id` and `withheld_at`.
- **No file of an `external = 1` row is ever deleted** by withhold, the stale re-check or any cleanup. Those paths delete the row only.
- **Project lost.** `refresh_projects` detects the loss and keeps own rows and files by design. It now also deletes the project's prepared rows with their (non-external) files, and its withheld rows.
- **Set unlinked** (`unlink_frame_set`). Prepared rows of frames no longer reachable through any other linked set of the project are deleted, with their files. Withheld rows are kept, so relinking keeps the member's decision.

### 4.3 Runs

All four run kinds go through the existing `publish_lock` / `claim_publish`: one run per project, a
second is refused. Each run gets a `publish_run_id`, a UUID string. The tracing field is
`publish_run_id`, a new entry in the logging spec's field dictionary: its `run_id` is the stacking
run's integer id.

| Kind | Takes | Does |
| ---- | ---- | ---- |
| `calibrate` | Ready frames (§4.1 order: no own row, not withheld, not in the Black Hole, gate passes, not prepared with a current recipe), optionally a `frameIds` selection | gate → A6 check (cached binding) → landing names → generation → write `c_*.fits` → insert `collab_prepared_frames` **right after each file's atomic rename**. **No seed, no announce, no hub call.** For an **attested** frame, "calibrate" is the stat/hash step only: the row gets `external = 1`, `calibrated_path NULL`, and the original's size and recipe. Nothing is written, and the member reviews the original. |
| `publish` | prepared frames, **plus** update-pending published frames — the `frameIds` selection, or every one of both when `frameIds` is absent | **re-gate**: a prepared frame that now fails is held back and keeps its row. **Re-check** each prepared frame: recipe current, file present at the recorded size. A stale frame loses its row (and, if not external, its file), returns to Ready and is reported as `stale`. Then seed by reference → announce → content versions → own rows → delete the consumed prepared rows. Update-pending frames regenerate as today (P6). |
| `republish` | as today (`frameIds`, guard dialog) | unchanged |
| `auto` | the worker's run for a project in a non-manual mode | `auto_calibrate`: `calibrate` over all candidates, then the update step for update-pending frames. `automatic`: `calibrate`, then `publish` over every prepared frame (also ones prepared earlier by hand), then updates. |

**Landing names and the own folder.**
- `new_frame_target` treats every `collab_prepared_frames.calibrated_path` as taken, alongside own rows and `taken_names`. Two candidates can then never land on the same `c_<stem>.fits`.
- **The own folder is pinned.** `publisher_dir` reads `own_dir` from the project's prepared rows, as it reads own rows. Without this, a peer with the same sanitized display name landing first would move "own" to `<name>_2` and orphan the prepared files.

**Known files.** A prepared file is a known file of the Collaboration root:
- `list_foreign_files` and the scanner's `reconcile_project_file` exclude prepared paths, as they exclude `landed_path`s. A prepared file is never "Other files" and never recorded as foreign.
- Nothing deletes it except Don't publish, the stale re-check, a re-calibrate that overwrites it, or project loss / unlink (§4.2). The storage engine deletes only quarantined rows, so it needs no change; a test pins that.

**Crash windows.**
- A crash between the rename and the row insert leaves a `c_*.fits` with no row. It shows under Other files until the next calibrate that allocates that name overwrites it. This is the same frame or a same-stem frame, and either way no row points at the old file.
- A crash during the write leaves a fits-writer temp file (`<name>.fits.tmp.<pid>.<seq>`). **The calibrate run deletes such temp files in the pinned own folder when it starts**, matching that exact pattern only. `.athtmp` is swept at mount and is not used here.
- A prepared row whose file is gone reads as stale (§4.1 step 5).

**A6.** `calibrate` checks the **cached** binding (`db::collab::publishing_device`), exactly as the
publish pre-check does today. It is refused with `Conflict(collab_publishing_device:…)` when the cache
names another device; the My frames refusal line and "Publish from this device" cover it. When the
cache is stale, the hub refuses at announce: the frames stay prepared and the refusal shows as today.

### 4.4 Gate changes

- **The gate's frame query is not filtered.** `project_gate` also feeds `list_project_own_frames` and `get_frame_set_project_status`. A filter would make published frames whose raw went to the Black Hole vanish from My frames and the counts.
- **Black Hole** (P5): an unpublished frame whose source file has a `black_hole` row is held back with failure kind `blackHole` (§4.1 step 3). It is never a `calibrate` candidate, and a prepared one is skipped by `publish`. A published frame is unaffected. A Black Hole add or restore re-dirties the projects of the frame's sets (`request_auto_publish_for_sets`).
- **Withheld**: reported as held back with kind `withheld` (§4.1 step 2). `calibrate` and `publish` skip it.
- **Already prepared**, recipe current: not a `calibrate` candidate.

### 4.5 Withhold and release — `set_collab_frames_withheld { projectId, frameIds, withheld }`

- **Withholding** (`withheld = true`):
  - It inserts withheld rows.
  - For a prepared frame, it deletes the prepared row and, if not external, the calibrated file in the same step. A delete failure logs at `error` and is returned.
  - It refuses frames that have an own row: published, pending, update pending or excluded (`Invalid`, naming the count).
- **Releasing** (`withheld = false`) deletes the rows and re-dirties the project for the worker.

Both log at `info` with `count`.

### 4.6 Modes and the worker

- **`set_project_publish_mode { projectId, mode }`** replaces `set_project_auto_publish`. `ProjectCard.autoPublish: bool` becomes `ProjectCard.publishMode: 'manual' | 'autoCalibrate' | 'automatic'` (serde camelCase of the DB values). `FrameSetProjectLink.auto_publish` becomes `publish_mode`.
- **`collab_autopublish.rs`**:
  - `drain_due_projects` keeps projects with `publish_mode != 'manual'` (instead of `auto_publish = 1`).
  - `run_publish_pass` runs kind `auto` with the mode.
  - Triggers are unchanged. A Black Hole restore and a Release are added.
- **Switching to a non-manual mode** dirties the project, so the worker runs after its debounce.
- **The worker runs only with the live exchange.** It is armed from `spawn_collab_live` (`runtime.rs` ~499-511). Signed out, or with no Collaboration folder, Auto-calibrate and Fully automatic do nothing.
  - The help line of both modes says: "Runs while Athenaeum is open and signed in to the hub."
  - The Project settings status box shows "Paused — collaboration is off" when the live state is `off` or `signedOut`.

### 4.7 Cancel — `cancel_collab_publish { projectId }`

- Cancelling sets the run's cancel flag. The compute-queue X sets the same flag.
- **While queued** for the compute slot: the run ends with outcome `cancelled`, not an error, and nothing changes.
- **While generating:** `run_publish_generation` checks the flag **between frames and breaks**. Today it checks only at the slot wait and inside the band loop.
  - The frame being generated fails cooperatively, as today. Its partial file is never left behind (atomic write), and its "calibration failed: cancelled" hold-back is **dropped**: it stays Ready.
  - Frames already written stay prepared (To review).
  - Untouched frames stay Ready.
  - No held-back entries are produced for the cancel.
- **During seed/announce:** the current batch completes, then the run stops. Announced frames are published; the rest stay prepared. A frame that was seeded but not announced is **untagged** with `unseed_project_frame` (`node.rs` ~3402-3431, as `unseed_all` does) before the run ends. Re-seeding the same bytes later is idempotent.
- The resolve phase also checks the flag.

## 5. Progress, outcome, snapshot

### 5.1 `collab-publish-progress` (event, both hosts)

```rust
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct CollabPublishProgress {
    pub project_id: String,
    pub publish_run_id: String,
    pub kind: PublishRunKind,        // calibrate | publish | republish | auto
    pub trigger: PublishTrigger,     // manual | auto
    pub mode: Option<String>,        // auto runs: the mode
    pub stage: PublishStage,         // queued | calibrating | seeding | announcing | versions
    pub current: u32,
    pub total: u32,
    pub current_file: Option<String>,
    pub started_at: String,          // RFC 3339
}
```

- Throttled to one per 300 ms per run. A stage change is sent at once.
- Logs are separate. Stage transitions log at `info` (`project_id`, `publish_run_id`, `stage`, `total`); per-frame logs stay at `debug`.
- It is a UI event, not a notification source (CLAUDE.md: never notify on `*-progress`).

### 5.2 `collab-publish-finished` (event, replaces `collab-published`)

It is emitted exactly once per run from a single exit path. This includes every early return after
the lock was taken (signed out, store unmounted, A6, outdated hub), which today skip the event, and
cancels. `refused` covers A6 and the outdated hub; `failed` covers everything else that ends the run
with an error.

```rust
pub struct CollabPublishFinished {
    pub project_id: String, pub publish_run_id: String,
    pub kind: PublishRunKind, pub trigger: PublishTrigger,
    pub outcome: PublishOutcome,     // done | cancelled | refused | failed
    pub calibrated: u32, pub announced: u32, pub updated: u32,
    pub failed: u32, pub stale: u32, pub held_back: u32,
    pub error: Option<String>,
    pub started_at: String, pub finished_at: String,
}
```

- The same JSON is stored in `collab_projects.last_publish_run`.
- `collab-published` is retired. Its two listeners move to this event: `ProjectDetail` reloads, and `useCollabNotifications` notifies. `docs/transfers/README.md` (events bullet) is updated.

### 5.3 Snapshot and cancel commands (both hosts)

- `get_collab_publish_run { projectId } -> { running: CollabPublishProgress | null, last: CollabPublishFinished | null }`. A page opened mid-run renders the panel at once.
- `cancel_collab_publish { projectId } -> ()`. When no run exists it is a no-op that logs at `debug`.

### 5.4 Notifications (`useCollabNotifications`, on `collab-publish-finished` only)

**One source.** A run's outcome is notified only from this event.
- `usePublishing` stops calling `notify()` for run outcomes and keeps its inline error line in My frames.
- A refusal returned by the command before a run starts (busy, A6 from the cache) stays inline, as today.
- The auto worker's A6 refusals stay quiet, as today: outcome `refused` with `trigger: auto` is history-only.

| Outcome | Title | Link |
| ---- | ---- | ---- |
| calibrate done, n > 0 | "Calibrated n frames in {title} — review them" | `/projects/{id}?tab=mine&segment=review` |
| publish/auto done, announced + updated > 0 | "Published n frames in {title}" | `?tab=mine&segment=published` |
| any, failed > 0 | detail "m failed — see Held back", `hasErrors` | `?tab=mine&segment=held` |
| publish/auto done, nothing sent, held back > 0 | "Nothing new to publish in {title}" (warning, as today) | `?tab=mine&segment=held` |
| cancelled | "Stopped in {title}" (info, history only: `toast: false`) | `?tab=mine` |
| refused, manual | "{device} publishes this project" (warning) | `?tab=mine` |
| refused, auto | same, history only (`toast: false`) | `?tab=mine` |
| failed | "Publishing failed in {title}" + error, `hasErrors` | `?tab=mine` |

A run where nothing happened (all zero, outcome `done`) stays silent, as now. The deep link gains a
`segment` parameter, handled like `tab` (applied, then removed from the URL).

## 6. Live sync

### 6.1 Confirmation reports (core)

Stamping lives where `Shared` lives. `FeedApplier` reports, `FeedWorker` forwards, and the runtime
loop records.

- **`FeedApplier::apply` returns a per-project report** alongside its effects: `ok`, `error`, and
  `SyncChanges`. `FeedWorker` sends it as a new
  `FeedOut::Synced { project_id, ok, error: Option<String>, changes: SyncChanges }`:
  - **after a `hello`:** one report per project in the hello.
    - `ok` when the project was in sync, or when both its catch-up and its holder hello-sync succeeded.
    - `ok: false` with the error when either failed. Today these are only logged.
    - The epoch-change branch reports each project it reloaded (ok) or could not reload (error).
  - **after a `versions`:** one report per project with a cursor that is in sync or caught up (ok), or whose catch-up failed (error). Projects without a cursor (not joined yet) are skipped, as today.
  - **after an applied `project` / `holders` / `resync` / `account` event:** ok for that project.
  - **a `FeedOut::Refused`** (403) also produces a `Synced { ok: false, error }` for that project.
- **`SyncChanges` counts what was applied:**
  - `manifestRows`: the row counts the manifest sync already computes (`collab_exchange.rs` ~627-645, `apply_inline` in `feed.rs` ~623-642), now returned instead of discarded;
  - `card`: the project snapshot changed (title, goals, thresholds, policy);
  - `members`;
  - `holders`.

  Presence is not counted; it stays on `collab-peers-changed`.
- **The runtime loop records `synced_at`** (UTC, in `Shared`, in memory only, no DB write) for every
  `ok` report.
- **`ProjectCard.syncedAt: string | null`** (RFC 3339) reads it through a `Shared` accessor, as
  `card_from_row` already reads live presence on both hosts. It is `null` when no live exchange runs;
  the pill then shows the status label. `fetchedAt` stays for its other uses.

### 6.2 `collab-project-synced { projectId, syncedAt, ok, error, changed }`

- The runtime loop emits it for each report, coalesced per project to at most one per second.
  - A new `SyncBurst` accumulator (shaped like `PeerBurst`, with a trailing flush) sums `SyncChanges`, keeps the last `ok` / `error`, and the newest `syncedAt` of an ok report.
  - `changed` = any count > 0.
- An `ok: false` report is never coalesced away: a window that saw a failure flushes with `ok: false`.

### 6.3 Sync now

- `sync_now` keeps `reset_all` + `reconnect_now` + the storage `Sweep`.
- **`DigestAll` moves behind the hello.** `Reconcile` now sets a `digest_after_hello` flag, and the feed worker runs `DigestAll` right after it applies the next `hello`. Holder claims are still checked, but the confirmation no longer waits behind one hub call per project.
- The confirmation a click waits for is the reconnect's `hello` report for this project.

### 6.4 The pill (`CollabLiveStatus variant="pill"`)

- **Label.** "Live · synced N s ago" counts from `syncedAt`. While connected it is normally at most
  about 60 s (the `versions` beat). It grows longer only while the feed worker is busy or retrying,
  which is the truth.
- **Click.**
  1. `collab_sync_now`, then **Syncing…** until a `collab-project-synced` for this project arrives
     after the click.
  2. **`ok: true`:** the age restarts from its `syncedAt`, and `onSynced` asks the page to re-read
     **detail, own frames, library and members**, unconditionally, and to bump `syncToken` (§6.5).
  3. **`ok: false`:** the pill stops at once and notifies "Sync did not complete — {error}"
     (`tone: 'warning'`).
- **If the status leaves `live`** (reconnecting, unreachable), the pill shows that status at once.
- **30 s with no report:** the pill stops and notifies "Sync did not complete — no answer from the hub".

### 6.5 Page reloads without a click

- `ProjectDetail` listens to `collab-project-synced`. On `changed`, its **own**
  schedule-if-none-pending timers run: 1 s for detail + library + members, 5 s for own frames. They
  are separate from the `collab-peers-changed` timers, which keep their current set.
- It also bumps a `syncToken`. `ModerationTab` and `ExchangeTab` load their own data, take the token
  as a prop, and re-fetch when it changes.
- `collab-peers-changed` is unchanged. `collab-publish-finished` reloads everything at once.

## 7. Overview

### 7.1 Layout (P9)

The `grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)]` grid stays:

- **Left column:** **My contribution**, then **Integration toward goal**.
- **Right column:**
  1. **Project settings** (new);
  2. **Needs attention**;
  3. **Exchange now**;
  4. **Quality thresholds**.

`MetaLine.tsx` and its test are deleted. The header keeps one row.

### 7.2 My contribution — four tiles (frontend only, derived from `own`)

Each tile is one button that opens its segment (clearing the state facet, as today) and shows:

- **Count** and label: `ready to calibrate`, `to review`, `published`, `held back`.
- **Meta line:** total integration `Σ exptimeSec` (`formatDurationPadded`), distinct nights, and size. Size is raw `byteSize` for Ready and Held back, `calibratedBytes` for To review and Published.
- **Per-filter hours,** in `filterOrder`, each with its `FilterDot`.
- **Footer:**
  - Ready: "Calibrate →".
  - To review: "Review and publish →".
  - Published: "N accepted · M pending".
  - Held back: the top three reasons with counts, e.g. "12 FWHM · 8 trailed · 3 withheld by you".

Rows with `exptimeSec = null` count as frames but add no time. A frame without a filter is labelled
exactly as the tables label it today (unset FILTER is a frame state, not an OSC convention).

### 7.3 Project settings card

Three groups, each with a small uppercase label and a visible help line (copy from the canvas):

1. **Publishing device.**
   - Shows "This device · {name}" with a live dot, "{device}", or "Nobody is publishing to this project yet".
   - When another device publishes, a **Publish from this device** button opens the existing switch confirm.
2. **Publishing.**
   - A three-way segmented control: Manual · Auto-calibrate · Fully automatic.
   - It commits on click (`set_project_publish_mode`), then the card is re-read. The shown value is always the stored one. A failure logs, notifies, and the control keeps the old value.
   - Below it, the selected mode's help line.
   - Then a status box: while a run is active, chip `auto`/`manual`, the stage title, a progress bar, the detail line and "Open My frames →"; otherwise the last run's one-line result from `get_collab_publish_run().last`.
3. **Auto-replicate.** Receivers only (`canReceive`). A real switch (`role="switch"`) with "On/Off" and its help line.

### 7.4 Needs attention

A new first-class item: "**N calibrated frames wait for your review**" → To review. The existing
items are unchanged.

## 8. My frames

### 8.1 Run panel

The panel sits above the segments. It renders from one hook, `useCollabPublishRun(projectId)`
(snapshot on mount, then `collab-publish-progress` + `collab-publish-finished`; StrictMode-safe
listener pattern).

- **Running:**
  - Chip `manual`/`auto`, the title ("Calibrating 12 of 48", "Fully automatic · seeding 31 of 48"), and the trigger ("you clicked Calibrate", "started after a scan").
  - The steps of this run kind with the current one highlighted: calibrate = Queued · Calibrate; publish = Seed · Announce; auto = Queued · Calibrate · Seed · Announce.
  - A bar, `current_file`, the elapsed time, and **Cancel**.
- **Finished:** one line from `last` (done / cancelled / refused / failed, counts, timestamp with seconds, trigger) with a link to the segment the result points at. It stays until the next run starts.

### 8.2 Segments and actions

The table is `ProjectFrameTable` as in wave 5.5 (§4.1 table). The new and changed `TableAction`s are:

- **Ready:**
  - **Calibrate** (primary): `calibrate_collab_frames { projectId, frameIds }`, then the run panel.
  - **Don't publish**.
  - **Blink**.
- **To review:**
  - **Publish** (primary): the existing `PublishConfirmDialog`. Its size is now the exact `Σ calibratedBytes` instead of an estimate.
  - **Don't publish**.
  - **Blink**.
- **Published:** **Update** for update-pending frames (`publish_collab_frames` with those ids), plus Republish, Exclude and **Blink**.
- **Held back:** **Release** for withheld frames, plus Solve, Analyze and **Blink**.

Blink is eligible only for frames whose file is on this device (§9.4). Like every action it follows
the "Verb N of M" eligible-subset convention.

## 9. Blink in a project

### 9.1 Sources

| Opened from | Entry shows | File | Frame |
| ---- | ---- | ---- | ---- |
| My frames · Ready / Held back | **raw** | catalog `files` row (`get_files_with_frames_by_ids`) | catalog `frames` row |
| My frames · To review / Published | **calibrated** (an attested frame: its original, shown as `raw`) | prepared `calibrated_path` / own `landed_path` | the source frame's catalog row (metadata + star metrics; calibration moves no pixel, debayer is off for publish) |
| Library · held here (`localState` `held` / `own_held`) | **replica** (own frames: calibrated) | `project_frames_local.landed_path` | none from the catalog; a frame built from the manifest fields (filter, exptime, date-obs, camera, telescope) with `id: null` |

An OSC calibrated file keeps `BAYERPAT` (`calibrated_generator.rs` ~620-631), so Blink shows it in
colour through the same VNG path as a raw OSC light.

**New command `get_collab_blink_frames { projectId, refs: CollabFrameRef[] } -> CollabBlinkEntry[]`**
(both hosts):
- `CollabFrameRef = { frameId?: number, frameUuid?: string }`.
- `CollabBlinkEntry = { key, source: 'raw' | 'calibrated' | 'replica', entry: FileWithFrame, frameUuid, sourceFrameId, publisherName, receivedFromMember }`.
- Core resolves every path from the DB, never from the client. Refs that are not on this device are dropped and counted in a `warn!`.

**New command `get_collab_frame_image { projectId, ref: CollabFrameRef, resolution? } -> JPEG bytes`**
(both hosts):
- Core resolves the path the same way.
- The desktop wrapper calls the existing `read_fits_image_bytes` (its cache and semaphore) and returns `tauri::ipc::Response`, as `get_master_light_preview` does (`commands/stacking.rs` ~265-300).
- The web route needs `routes/images.rs` (~49-165, inline today) refactored first into a `(state, path, resolution)` helper that both `get_frame_preview` and the new route call.
- Instrumented at `level = "debug"` (hot path).
- Blink uses it for every `calibrated` and `replica` entry. `raw` entries keep today's path.

### 9.2 `BlinkViewer` changes

`BlinkViewer` keeps its `frames: FileWithFrame[]` prop for every existing caller. A project caller
passes entries that carry a little more, plus actions.

- **`BlinkFrame = FileWithFrame & { key?: string; imageRef?: { projectId: string; ref: CollabFrameRef }; source?: 'raw' | 'calibrated' | 'replica'; badge?: string }`.**
  - Blink snapshots the array when it opens. Project actions never remove entries, so indexes stay stable while it is open. Index-keyed caches stay as they are.
  - The list key becomes `key ?? file.id ?? index`, which fixes the `id: null` collision.
  - **Both** image load sites (the preview load and the full-res load) switch on `imageRef`: when it is set, they call `get_collab_frame_image`.
- **`actions?: BlinkAction[]`**, its own type, not `TableAction`:
  `{ id: string; label: (n: number) => string; eligible: (f: BlinkFrame) => boolean; tone: 'default' | 'warn' | 'danger'; run: (frames: BlinkFrame[]) => Promise<void> | void }`.
  - When given, the Black Hole / Restore buttons, their confirm and `get_blackholed_file_ids` are not used at all.
  - "Locate in file browser" is hidden: Collaboration-root paths are not browsable there.
  - Each action shows its eligible count of the selection ("Don't publish (2)"). An action with zero eligible entries is hidden.
  - After an action, the caller passes a fresh `frames` array with the same keys and updated `badge`s (e.g. `withheld`, `excluded`). Blink matches them by `key` and keeps its position, selection and caches.
- **`contextLabel?`** fills the strip under the toolbar:
  - the source chip;
  - the file name;
  - a hint ("exactly the file that will be published", "received from {member} · {time}");
  - a role hint.
- **`viewOnly?`** renders the "View only" chip.
- **Overlays.**
  - Blink joins the wave 5.5 overlay stack (rule R35), so a dialog opened above it owns Escape and focus.
  - Its `window` keydown handler ignores events whose target is an input, textarea or contenteditable, and every key while an overlay above it is open.

Every existing caller is unchanged: no `actions` means the Black Hole, as today.

### 9.3 Actions by context and role

| Context | Action(s) |
| ---- | ---- |
| own Ready / To review | **Don't publish (n)**; **Release (n)** for withheld entries |
| own Held back (withheld) | **Release (n)** |
| own Published, moderator | **Exclude from project (n)** |
| own Published, member | none (view only, hint "Only a moderator can exclude published frames.") |
| Library, moderator | **Exclude from project (n)** for published, non-excluded frames |
| Library, member | none (view only) |

**Exclude** opens the existing `ExcludeDialog` (reason 1..500 chars) on the overlay stack above Blink.
It takes `FrameVM[]`: the page keeps a `key → FrameVM` map for the entries it opened Blink with and
passes the matching VMs. On success the entries get the `excluded` badge, and the page reloads.

**Don't publish** asks for confirmation only when the selection holds prepared frames, because their
calibrated files are deleted. After Don't publish / Release, Blink keeps the entries and shows their
new state chip. Its `onFramesRemoved` contract is unchanged.

### 9.4 Blink eligibility in the tables

| Source | Eligible when |
| ---- | ---- |
| raw | `path != null` and the source is not in the Black Hole |
| calibrated | `calibratedPath != null` and `localState` is not `own_missing` (an `own_changed` file opens with a "changed on disk" badge) |
| replica | `localState` is `held` or `own_held` |

The action label follows the eligible-subset rule: "Blink 3 of 5", "Blink all 48".

## 10. Commands and events

| Change | Command / event | Hosts |
| ---- | ---- | ---- |
| new | `calibrate_collab_frames { projectId, frameIds? } -> PublishResult` | both |
| changed | `publish_collab_frames { projectId, frameIds? }`: publishes prepared + update-pending frames; Ready ids are refused per frame with a reason | both |
| new | `set_collab_frames_withheld { projectId, frameIds, withheld } -> u32` | both |
| replaced | `set_project_auto_publish` → `set_project_publish_mode { projectId, mode }` | both |
| new | `get_collab_publish_run { projectId }` | both |
| new | `cancel_collab_publish { projectId }` | both |
| new | `get_collab_blink_frames { projectId, refs }` | both |
| new | `get_collab_frame_image { projectId, ref, resolution? }` | both |
| new event | `collab-publish-progress`, `collab-publish-finished`, `collab-project-synced` | both |
| retired event | `collab-published` | both |

**Command semantics:** `calibrate_collab_frames` and `publish_collab_frames` await the run, as
`publish_collab_frames` does today. Refusals (busy, A6, outdated hub) return as errors, and progress
arrives through the events meanwhile.

**Command count:** 288 + 6 new = **294** (one replaced, not added). CLAUDE.md's count line is updated in
the same change. New model types go into the `ts_export.rs` registry. The TS mirrors in
`src/types/models.ts` are regenerated (`ProjectCard.publishMode`, `ProjectCard.syncedAt`, the
`OwnFrameRow` additions, the new payloads).

## 11. Frame set Project block

`FrameSetProjectLink.counts` gains `prepared` and `withheld`. The summary adds "N to review" and
"N withheld". `autoPublish` becomes `publishMode`, which is shown only as text.

## 12. Testing

**Core** (`cargo test -p athenaeum-core --all-targets` before any push):

1. **Migration.**
   - Existing rows become `manual`.
   - The column default is `manual`.
   - `auto_publish` is no longer read.
2. **`calibrate`.**
   - It writes `c_*.fits` and prepared rows; each row is inserted right after its file's rename.
   - An attested frame gets an `external` row, and no file is written.
   - The own folder is pinned: a peer landing first with the same display name does not move it.
   - Stale fits-writer temp files in the own folder are removed at start, and nothing else is.
   - Prepared paths are absent from `list_foreign_files` and never recorded as foreign by the scanner.
   - It makes **no** seed or announce call; the fake hub records none.
   - Recipe and size are recorded.
   - Landing names avoid prepared paths.
   - It is refused under a foreign cached A6 binding.
3. **`publish`.**
   - It consumes prepared rows and announces them.
   - A stale recipe or a missing file drops the prepared row back to Ready, reported as `stale`.
   - Update-pending frames regenerate.
   - Ready ids are refused.
   - A prepared frame that now fails the gate is held back and keeps its row.
   - A stale external row is dropped without touching the original file.
4. **Withhold.**
   - A withheld frame is never a calibrate or publish candidate.
   - Withholding a prepared frame deletes its file and row; an external one keeps the original.
   - Project loss deletes prepared rows and files; unlink deletes only frames no longer reachable through another linked set.
   - Withholding a published frame is refused.
   - Release re-dirties the project.
5. **Black Hole.**
   - A black-holed unpublished source is held back with kind `blackHole` and is not a candidate.
   - A published frame whose raw is black-holed stays in My frames.
   - A restore makes it a candidate again.
6. **Cancel.**
   - Queued → `cancelled`, nothing changed.
   - Between frames → the rest stay Ready, with no `calibration failed: cancelled` held-back entries.
   - The in-flight frame's "calibration failed: cancelled" hold-back is dropped.
   - Mid-announce → the batch completes; seeded-but-unannounced frames are untagged.
7. **Events.**
   - Progress stages arrive in order and are throttled.
   - Exactly one `collab-publish-finished` per run, on every exit path, including signed out, unmounted, A6 and outdated.
   - `last_publish_run` is persisted.
8. **Worker.** `manual` projects are never drained. `auto_calibrate` never announces a new frame. `automatic` calibrates and publishes.
9. **Known files.** Neither the scanner's Collaboration-root reconcile nor the storage engine deletes or flags prepared files.
10. **Live.**
    - `hello` / `versions` / applied events produce ok reports and stamp `synced_at`.
    - A failed catch-up or a 403 produces `ok: false` and does not stamp.
    - The epoch-change branch reports each project.
    - `SyncBurst` sums changes, never coalesces a failure away, and flushes trailing.
    - `changed` follows the manifest row counts, not `NeedSetChanged`.
    - After `sync_now`, `DigestAll` runs after the hello, not before.
    - `ProjectCard.syncedAt` is `null` when the live exchange is off.
11. **Blink commands.**
    - Paths come from the DB.
    - Refs not on the device are dropped.
    - The web image route answers JPEG for a held replica.
    - A ref outside the project is refused.
12. **`ts_contract`** for every new type.

**Frontend** (vitest + tsc):
- the tiles' derivation (hours, nights, filters, size source, held-back reasons);
- the Project settings card (mode commit, failure keeps the old value, help line, run status);
- the run panel (snapshot mid-run, progress, Cancel; nothing once the run ends — N16);
- the pill (waits for the synced event; `ok: false` stops it with the error; 30 s timeout; re-reads all four loaders and bumps `syncToken`);
- the page reload on `changed`, with its own timers, plus the Moderation / Exchange re-fetch on `syncToken`;
- Blink with `actions` (no Black Hole and no "Locate in file browser", eligible counts, view-only chip, both image loads through `get_collab_frame_image`, key-stable badge updates, keydown ignored while `ExcludeDialog` is open above it, Escape closes only the dialog);
- Blink eligibility labels in both tabs;
- notifications per outcome, and `usePublishing` no longer notifying run outcomes;
- the `segment` deep link.

**Harness:** `npm run ui:harness` gains a `review` scenario (prepared frames + a running calibrate)
and a side-by-side check against the canvas.

**Real app:** the owner smoke list goes to `docs/superpowers/open-items.md`:
1. a manual calibrate → review in Blink → Don't publish one → publish;
2. Auto-calibrate after a scan;
3. Fully automatic;
4. cancel at each stage;
5. Live click with a change made on another device;
6. Blink as moderator and as member;
7. a migrated project shows Manual.

## 13. Delivery

- **Wave 1 — core and both hosts.**
  - Storage + migration, run kinds, gate changes, withhold, cancel, events, snapshot.
  - Touch list for the mode rename: the 13 `CollabProjectRow { auto_publish }` literals, `SELECT_COLS` and its index-based reader in `db/collab.rs`, and `FrameSetProjectLink`.
  - Live confirmations and the synced event.
  - Blink commands.
  - ts export.
  - Rust tests.
- **Wave 2 — frontend.**
  - `useCollabPublishRun`, the Overview layout + Project settings card + tiles.
  - The run panel + segments/actions.
  - The pill and page reloads.
  - Blink `actions` / `imageRef` / `contextLabel` and the table Blink actions.
  - Notifications, deep link, frame set Project block.
  - The harness fixture (`scripts/ui-harness/fixtures.mjs` `autoPublish` → `publishMode`).
  - vitest.
- **Wave 3 — references.**
  - Harness scenario. The harness SSE is idle, so the "running" state comes from the
    `get_collab_publish_run` snapshot fixture.
  - The logging spec's field dictionary: `publish_run_id`.
  - `docs/transfers/README.md` (collab publish section).
  - CLAUDE.md command count + the Transfers/collab bullet.
  - `docs/frontend/notifications.md` (new outcomes).
  - Release-note line for P1.
  - open-items smoke list.

Merging and pushing wait for the owner's word. `wave-5.5-project-ui` must merge first, or together.

## 14. Out of scope

- "Ask to exclude" for members (needs a hub endpoint and a moderation queue entry).
- Per-frame Download / Stop keeping in Library (separate cycle, needs the want/unwant model).
- Review of updates to already-published frames (P6).
- A disk-space preflight for Calibrate: no longer out of scope — see §16.1 N40.
- Found while researching, not fixed here: Republishing an `own_missing` frame that regenerates to identical bytes deletes the temp file and does not restore the landed file (`collab.rs` around the identical-bytes branch). It goes to open-items for its own check.

## 15. Review record (fable, 2026-10-01)

| # | Finding | Resolution |
| ---- | ---- | ---- |
| 1 | Attested sets have no calibrated file; withhold / stale would delete originals; UNIQUE path breaks across projects | `external` prepared rows, never unlinked; uniqueness per project (§4.2, §4.3, §9.1) |
| 2 | Confirmation plumbing: `FeedApplier` has no `Shared`; `DigestAll` runs ahead of the hello; per-project failures swallowed, so a refused project never confirms | `FeedOut::Synced` reports with `ok` / `error`; the runtime loop records; `DigestAll` after the hello; the pill stops on `ok: false` (§6.1–6.4) |
| 3 | `changed` cannot come from `FeedEffect` | `SyncChanges` from the manifest row counts; a `SyncBurst` accumulator (§6.1, §6.2) |
| 4 | Filtering the gate query by Black Hole hides published frames | No query filter; held-back kind `blackHole` for unpublished frames only (§4.4) |
| 5 | Landing names, own-folder drift, foreign listing, temp files; §3 `delete_not_in` claim wrong | Own folder pinned; prepared paths excluded from foreign; temp sweep at calibrate start; §3 corrected (§3, §4.3) |
| 6 | Blink feasibility: keydown vs dialog, `TableAction` shape, index-keyed state, Locate, OSC colour, Response / web helper | `BlinkFrame` + own `BlinkAction` type, overlay stack + keydown guard, Locate hidden, both load sites, desktop Response, web helper refactor (§9) |
| 7 | State derivation precedence and the touch list | Derivation order + touch list (§4.1) |
| 8 | A6 at calibrate is the cached binding | Stated; a stale cache refuses at announce, frames stay prepared (§4.3) |
| 9 | Notifications double up; auto refusals would toast | One source; outcome `refused`, history-only for auto; the "nothing new" row kept (§5.2, §5.4) |
| 10 | Modes need the live exchange | Help text + "Paused — collaboration is off" (§4.6) |
| 11 | Leave / unlink hooks | Project loss via `refresh_projects`; unlink only for unreachable frames (§4.2) |
| 12 | Cancel open question | Untag with `unseed_project_frame`; in-flight hold-back dropped (§4.7) |
| 13 | Contract nits (`run_id` name, `fetchedAt`, emit points, timers, self-loading tabs, literals, fixture, grid class, `own_changed`, README) | Each fixed in place (§1, §3, §5, §6.5, §7.1, §9.4, §13) |

## 16. Amendments (wave 1, 2026-10-01)

Plan rulings:

| # | Ruling |
| ---- | ---- |
| W1 | `CollabPublishFinished` carries `calibrated, announced, updated, stale, heldBack`, with no `failed` field. A run's `heldBack` counts only frames the run attempted, or frames the member selected. A gate failure of an unselected frame is the Held back segment's business, not the run's: in `only = None` runs, gate-failing rows are no longer pushed into `PublishResult.held_back`. |
| W2 | A prepared row also stores `frame_uuid` (the catalog `frames.uuid`), `xxh3` and `size_mtime_seen`. A prepared frame is current when its recipe equals `current_recipe_for_frame` and its file's `size_mtime_seen` equals the stored one. |
| W3 | Withholding a frame while a run of its project is active never deletes files. Two places act instead: the run drops withheld frames before each announce batch and when writing; and `RunHandle::finish` deletes prepared rows and files of frames withheld in the meantime. |
| W4 | Cancel is checked before each calibrated frame, before seeding starts and before each New-frame announce batch. Once the update step (Update/Adopt seeding + versions) has begun, it runs to its end. |
| W5 | `PublishMode` lives in `db::collab`. The DB values are `manual` / `auto_calibrate` / `automatic`; the wire values are `manual` / `autoCalibrate` / `automatic`. The `ALTER ... DEFAULT 'manual'` fills every existing row, which is P1's one-time migration. |
| W6 | The web `get_collab_frame_image` route lives in `routes/collab.rs` and calls a `routes/images.rs` helper `render_path_jpeg(state, path, resolution)`, extracted from `get_frame_preview`. |
| W7 | A queued-then-cancelled run is `Ok` with outcome `cancelled`; it no longer returns `Internal("publish: the compute slot wait was cancelled")`. |
| W8 | `get_collab_frame_image` takes its frame as `frame` on the wire (`{ projectId, frame, resolution? }`), not `ref`: `ref` is a Rust keyword and would break the Tauri argument mapping. `CollabBlinkEntry` carries `publisherName` but no received-from fields; the Library row (`ProjectFrameView`) already has `receivedFromMember` / `receivedAt` for the Blink hint. |
| W9 | Calibrate runs the credentials / node / store checks of `run_publish` unchanged, so it needs a signed-in account. None of those checks makes a hub HTTP call. A signed-out member cannot calibrate, just as they cannot publish. |

Controller rulings and accepted review deviations:

- R1: tests touching the auto-publish dirty statics hold `collab_autopublish::test_lock()`.
- R3: the operation-queue worker exits once every handle is dropped and its queue is drained (production keeps a handle for the app lifetime); this fixes test-context thread leaks that hit the macOS 4096-thread cap.
- R4: remaining test thread pressure (r2d2 pool reaper threads, up to 30 s; iroh-blobs 0.103 store leaks one permanent thread per store) is not fixed this cycle; the margin was about 500 at the full-suite gate; open-items entry.
- R5: `RunScope::Republish` never produces a New plan; a selected never-published frame is held "not published yet — calibrate and publish it first"; adopt-class frames are unchanged.
- R6: an `epoch_changed` Conflict produces no sync report; the reconnect's hello reports every project.
- Calibrate: `record_prepared` returns `Result<bool, String>` (a failed row write holds the frame back and removes its fresh non-external file); adopt-class frames are never Calibrate's (skipped after pass 1, their landing reserved in `claimed`); the stale drop and the writer-temp sweep run only after every refusal check, so a refused run touches no disk.
- Publish: a failed withheld read holds that announce batch back (fail-closed) instead of erroring mid-announce; a prepared frame whose file name was taken since its calibrate is dropped as stale; Publish itself drops withheld prepared frames (row and non-external file) as well as the run's end (W3); an adopt-class frame's prepared row is consumed on adoption (its file kept only when it is the adoption landing); another frame's prepared file sitting on an adoption landing is dropped as stale before the adopt writes there.
- Cancel: a cancel landing after the last regenerated frame is caught before seeding (`outcome.cancelled || run.cancelled()`).
- Run tracker: an interrupted run (dropped future or panic) emits and persists one `failed` finished event ("run interrupted"), sets the cancel flag and drops withheld prepared frames; `finish` is idempotent and unregisters before emitting.
- Auto worker: claims the publish lock before reading the mode.
- Live sync: `SyncChanges.manifest_rows` is the rows written or pruned by the manifest sync (not change kinds, so a moderator restore now counts); `card` is set by any meta/thresholds/dictionary refresh; reports merge per project (one honest report per project per apply; a 403 is merged into it); `epoch_change` catches up a project already on the new epoch whose head is ahead (no bare ok); `FeedWork::DigestAll` is replaced by `DigestAfterHello`; `sync_now` sends `Reconcile` before `reconnect_now` so the digest flag is always queued before the new stream's hello.
- Blink: the wire arg of `get_collab_frame_image` is `frame` (W8); a frame id outside the project's linked sets refuses the whole `get_collab_blink_frames` call (Forbidden); only "not on this device" (NotFound) drops an entry, other errors propagate; a prepared file is served only when `prepared_is_current`; an own frame reached by uuid shows calibrated (raw when attested); uuid paths must be inside the current Collaboration root (A5).
- §3 and §6.3 wording: read the after-hello digest wherever `DigestAll` is named.
- `CollabPublishFinished` fields: calibrated, announced, updated, stale, heldBack (no `failed`, W1).
- Commands: six added (`calibrate_collab_frames`, `set_collab_frames_withheld`, `get_collab_publish_run`, `cancel_collab_publish`, `get_collab_blink_frames`, `get_collab_frame_image`); `set_project_auto_publish` renamed `set_project_publish_mode`. Events: `collab-published` retired; `collab-publish-progress`, `collab-publish-finished`, `collab-project-synced` added.

Final whole-branch review fixes:

- Unlink: `unlink_frame_set` is refused with `Conflict(PUBLISH_BUSY_MSG)` while a publish-family run of the project is registered or holds the publish lock (warn, `outcome = "publish_busy"`); it holds the lock for the whole unlink, so a prepared file a run already seeded is never deleted under it.
- Gate count: `GateReport.publishable` (→ `ProjectCard.publishable`) leaves out a withheld frame and one whose raw is in the Black Hole unless an own row exists (§4.1 steps 1–3); `total` and `rows[].publishable` stay the unfiltered gate (§4.4).
- Run end order: `RunState::conclude` is unregister → drop withheld prepared frames → persist → emit, for `finish` and the interrupted-run guard alike; `set_collab_frames_withheld` reads the run registry inside its `BEGIN IMMEDIATE`, so a withhold that saw the run registered has committed its row before the end-of-run `DELETE`.
- No double panic: `Database::try_conn()` returns the pool error instead of panicking; the run's end (persist and the withheld drop) uses it and logs at `error`, so the guard can drop during a panic unwind without aborting.
- Withhold scope: withholding refuses ids that are not a LIGHT of a set linked to the project (`Invalid`, "N of these frames are not in this project's linked frame sets"), checked inside the transaction; Release is not restricted (it only deletes this project's rows).
- W1: a withheld frame the member selected for Publish is reported in `heldBack` with "Withheld by you" (its prepared row and file still go).
- Pre-seed re-stat: a prepared New frame's file is re-stat'd (`size_mtime_seen`) right before it is seeded; a changed or missing file is stale — row and non-external file dropped, counted in `stale`, never seeded or announced with the calibrate-time `xxh3`/size.
- Blink: by `frameId`, a prepared file is served only when the frame is not withheld and its raw is not in the Black Hole (§4.1 steps 2–3, as My frames shows it); otherwise the frame resolves raw.

## 16.1 Amendments (wave 2, frontend)

Plan rulings:

| # | Ruling |
| ---- | ---- |
| F1 | **Update** (published frames with `contributorState === "updatePending"`) runs `publish_collab_frames` with those ids directly, with no confirm. The frames are already in the project, and the run panel shows the work. |
| F2 | **Don't publish** asks for confirmation only when the targets include prepared (To review) frames, because their calibrated files are deleted. From Ready it acts at once. |
| F3 | The run panel's step strip is **monotonic per run**. An `auto` run re-entering `queued` / `calibrating` for its update step (a wave-1 leftover) never moves a finished step back. |
| F4 | A refusal returned by a command before a run starts (busy; A6 from the cached binding; outdated hub) stays where it is today: inline line, `updateRequired` banner, A6 refusal box, busy info toast. Every outcome of a started run is notified **only** from `collab-publish-finished` (§5.4). `usePublishing` stops toasting run failures. |
| F5 | The Live pill shows the newest of the card's `syncedAt` and every `collab-project-synced.syncedAt` it hears for its project, so the age restarts on each confirmation without a card re-read. |
| F6 | Unlinking a set while a run is active returns the busy text from core. `LinkObjectDialog` shows "A publish run is in progress — try again when it ends." for that error. |
| F7 | Blink's canvas height is measured from the real header (toolbar plus context strip) with a `ResizeObserver`, replacing the hard-coded `window.innerHeight - 48`. |
| F8 | A run that calibrated frames and sent none (a calibrate run, or an `auto` run in Auto-calibrate mode) notifies "Calibrated n frames in {title} — review them" and links To review. The spec's "calibrate done" row covers both kinds: in Auto-calibrate, the review notice is the one sign that work was done. A `refused` outcome whose error is an A6 refusal (`collab_publishing_device:<name>`) reads "{device} publishes {title}", never the raw code. |
| F9 | The run panel's step strip follows the run kind: calibrate = Queued · Calibrate; publish = Seed · Announce; republish and auto = Queued · Calibrate · Seed · Announce. `versions` lights Announce. A publish run that regenerates an update (`queued` / `calibrating`) shows no lit step, and its title carries the stage. |

Contract notes and execution deviations:

| # | Note |
| ---- | ---- |
| N1 | `BlinkFrame.imageRef.frame` (not `.ref`, W8); `contextLabel` has the signature `(f) => string`; `SegmentTiles` gained a `sub` line. |
| N2 | Overview Published footer: "N accepted" counts `pubState === 'published' && accepted !== false`. `OwnFrameRow.accepted` is the exclusion flag and is true for pending rows too (core: "published && accepted"); accepted and pending partition the segment. |
| N3 | Overview Held back footer groups per gate kind (`REASON_LABEL`), not per threshold rule; count ties break by `HELD_KIND_ORDER` (P1). An unset FILTER is labelled as the tables label it (empty), with no invented label. |
| N4 | The My frames tab badge shows only the non-zero parts: "N ready", "M to review", or both joined by " · ". |
| N5 | Project settings card: "Paused — collaboration is off" for live state `off` or `signedOut`; this device with no hub name reads just "This device"; the auto-replicate switch's accessible name is its On/Off label. |
| N6 | Run wording lives in one module, `publishRunText.ts` (`STAGE_TITLE`, `MODE_LABEL`, `describeLastRun`), used by the settings card and the run panel. |
| N7 | Don't publish and Release are one hook, `useWithhold`, shared by My frames and project Blink. |
| N8 | The publish confirm shows the exact sum of `calibratedBytes` (fallback `byteSize`), no "≈ estimate"; sizes are decimal (`formatSize`). |
| N9 | Live pill: "Syncing…" only while live or connecting; any other status (reconnecting, unreachable, outdated, signedOut) shows at once and the wait continues. The failure reason is in the notification title ("Sync did not complete — {error}" / "— no answer from the hub") because toasts show titles only. The pill reads `ProjectCard.syncedAt`, not `fetchedAt`. |
| N10 | Blink: the snapshot of `frames` is taken only in project mode (`actions !== undefined`); Blink joins the overlay stack for every caller; its key guard skips text-taking inputs only (the toolbar's range slider keeps driving Blink), and an event a handler above already cancelled (`defaultPrevented`) is ignored, so Alt/⌘+←/→ no longer change speed (the app's global navigation cancels those chords). |
| N11 | Project Blink: `ProjectBlink` mounts `BlinkViewer` in a body portal only after `get_collab_blink_frames` resolves (a fresh mount per open); entries are matched by core's key `frameUuid ?? f<frameId>`; actions read the CURRENT rows by key, so badges and eligibility follow a reload. |
| N12 | Harness: the `review` scenario (48 calibrated frames, three withheld, a running calibrate at 12 of 48) is served through the `get_collab_publish_run` snapshot because the harness has no event channel. |
| N13 | Final review: the publish confirm and the republish guard close the moment the user confirms and the command runs detached, so the run panel (§8.1) and its Cancel are reachable for the whole run (`publish_collab_frames` awaits the run). A refusal returned before the run starts shows in My frames — inline line, `updateRequired` banner, A6 box, busy toast (F4); the dialogs carry no busy or error state. |
| N14 | Final review: the Live pill's wait (§6.4) compares server stamps only, never the browser clock. At the click it keeps `last` = the newer of the card's `syncedAt` and the newest stamp heard; the first ok report for the project stamped after `last` ends the wait (any stamped ok report when `last` is null). Not-ok reports and the 30 s timeout are unchanged. Cost: a report emitted just before the click but delivered after it ends the wait early. |
| N15 | Final review: a `done` run whose only effect is `stale > 0` notifies a warning "n frame(s) changed since calibration in {title} — back to Ready", linking `?tab=mine&segment=ready`; beside a publish or a calibrate the count joins the detail as "· n back to Ready" (§5.4). |
| N16 | Owner review 2026-10-01: the My frames run panel shows a run only while it runs. The finished-run line (outcome, counts, "Open Published →") is gone from My frames: the segment tiles below show the result, the outcome is notified (§5.4) and the Overview's Project settings card keeps the last-run line (§7.3). Supersedes the "Finished" bullet of §8.1. |
| N17 | Owner review 2026-10-01: Exclude never acts on the whole view. With nothing selected it is disabled and reads "Exclude" (title "Select the frames to exclude"); with a selection it follows the eligible-subset label. `TableAction.needsSelection` carries the rule; My frames · Published and Library use it. |
| N18 | Owner review 2026-10-01: the table's column picker moved from the group row into the right end of the table's header row (an icon button, "Columns"), outside the scroll box so its popover is never clipped. |
| N19 | Owner review 2026-10-01: Blink's strip hint only says what the source chip and the file name do not — "received from {member} · {time}" for a replica, "exactly the file that will be published" for a To review entry (calibrated, or an attested original) — plus the view-only role hint. The generic "the calibrated file of this frame" / "the raw frame on this device" are gone. |
| N20 | Owner review 2026-10-01: the docked side panel ends 24 px (the page's bottom padding) above the viewport, not 16 px; the old gap made a short tab 8 px taller than the screen, so opening a member's panel grew a page scrollbar that kept refitting the panel. |
| N21 | Polish 2026-10-01: `useCollabPublishRun` registers both listeners first and reads `get_collab_publish_run` only once both `listen` calls resolved. A snapshot `running` whose `publishRunId` was already heard in a `collab-publish-finished` is ignored, and `last` keeps the newer of the heard outcome and the snapshot's (by `finishedAt`). |
| N22 | Polish 2026-10-01: a snapshot with `running: null` resets the running run and the reached step; a `projectId` change resets running, last and the reached step before the new project's snapshot. Follow-up: a `running: null` reply keeps the run when a `collab-publish-progress` heard while the read was pending names a run not yet heard finishing (it was queued after the reply was computed). |
| N23 | Polish 2026-10-01: a `done` run whose only effect is `stale > 0` reads "N frame(s) back to Ready — changed since calibration" in the last-run line (tone warn, segment Ready), agreeing with the N15 notification. |
| N24 | Polish 2026-10-01: a manual `refused` run on an outdated hub notifies "Not published in {title} — this hub needs a newer Athenaeum" (toasts show titles only); the detail keeps the update sentence. |
| N25 | Polish 2026-10-01: the last-run line drops the outdated-hub sentence's full stop, so it reads "… update to publish · {time} · {trigger}", never ". ·". |
| N26 | Polish 2026-10-01: the Overview Held back footer gives one reason per frame, its first failure; a `threshold` failure is named by its first failing rule's `label` (`RuleVerdict.pass === false`: "FWHM", "trailed", …), else the kind label — the §7.2 example "12 FWHM · 8 trailed · 3 withheld by you". Ties break by `HELD_KIND_ORDER`, then by label. Supersedes N3. |
| N27 | Polish 2026-10-01: a My contribution tile's accessible name is "N label" with N formatted as shown (`toLocaleString('en-US')`), and it is described (`aria-describedby`) by its meta line and footer. |
| N28 | Polish 2026-10-01: the Publishing mode control is a named group ("Publishing mode") and is disabled, with the auto-replicate switch, while a write is in flight. The switch keeps its visible On/Off label and is named "Auto-replicate" (`Checkbox.ariaLabel`). Supersedes N5's last clause. |
| N29 | Polish 2026-10-01: the last-run line colours a `warn` outcome with `text-warning` (`error` stays `text-error`). |
| N30 | Polish 2026-10-01: an Exclude from My frames — the Published table's or Blink's "Exclude from project" — re-reads own frames and the Library rows (`MyFramesTab.onExcluded`). |
| N31 | Polish 2026-10-01: the Live pill's `collab-project-synced` listener cleanup also ends Syncing… and forgets the stamps heard for the previous project, so a project change mid-wait starts from the new card. |
| N32 | Polish 2026-10-01: Blink's "changed on disk" badge is an own frame's only (`own.localState === 'own_changed'`); Library blinks only `held` / `own_held` replicas, so its `own_changed` branch is gone. Follow-up: the Library branch (`lib.localState === 'own_changed'`) is back, because Blink reads the current row and an own frame blinked from the Library as `own_held` can turn `own_changed` on a reload while Blink is open. |
| N40 | Owner-approved 2026-10-01: a free-space check before calibrated files are written. `api::collab::run_publish_generation` checks once, after the attested/generated split and before the compute permit, the first prepared row or any file — so it covers Calibrate (manual and the auto worker), Publish of update-pending frames and Republish. Only generated frames count (an attested-only run writes nothing and is never checked). Estimate per frame: `naxis1 × naxis2 × 4 + 16 KiB` (float32, one plane — publish never debayers, OSC stays a CFA mosaic) from the catalog; without both dimensions `2 × files.size`; with neither, 0 (logged at debug). Needed = the sum + a fixed 1 GB reserve (`CALIBRATE_SPACE_RESERVE_BYTES` = 1 000 000 000 — the disk is never filled to zero). Free = `disk::free_bytes` of the own folder (its nearest existing ancestor before the first write; `statvfs` on unix, `GetDiskFreeSpaceExW` free-to-caller on Windows). Free < needed refuses the WHOLE run with `Conflict("collab_no_space:<needed>:<free>")` (decimal bytes), outcome `refused` with that message as `error`; equal passes. An unknown free space (`None`) is a `warn!` and the run proceeds. The auto worker logs the refusal and does not re-queue it: it runs again on the project's next trigger. The probe never measures above the Collaboration root (a missing root is unknown, not another volume's figure). The check is a preflight measured before the compute-slot wait, not a reservation: a job admitted earlier may still consume the space. |
| N41 | Follow-up 2026-10-01, the frontend half of the free-space check (core refuses a run that would calibrate frames with `collab_no_space:<needed>:<free>`, decimal bytes, `needed` incl. the 1 GB reserve): a calibrate, publish or republish rejected with it shows `noSpaceText` — "Not enough free space for the calibrated frames: X GB needed (incl. 1 GB reserve), Y GB free on the Collaboration folder's disk." — on that action's line in My frames and raises no toast of its own (F4). The run's `refused` outcome notifies "Not enough free space to calibrate in {title}" (warning, `hasErrors`, `?tab=mine`) and toasts for a manual and an auto run alike; an auto refusal carries `dedupeKey` `collab-no-space-{projectId}-{local YYYY-MM-DD of finishedAt}`, so a worker refused on every scan notifies once a day per project. The last-run line reads "Not run — not enough free space (X GB needed, Y GB free)" (tone warn). Sizes are decimal GB with one decimal. |
