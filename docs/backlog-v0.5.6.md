# Backlog — v0.5.6

Opened 2026-09-06, the day v0.5.5 was tagged. Items 2–4 are carried over from
`docs/backlog-v0.5.5.md` by owner decision — they were deferred, not dropped.
Item 1 came out of the v0.5.5 release itself. Items 6–8 were raised by the
owner on 2026-09-07; item 6 was dropped the same day, after the analysis it
asked for.

Each entry states what is already *known* — the code that owns the behaviour,
what was measured — and the question that has to be answered before it can be
planned.

## 1. The plate-solve gate thresholds need controls — SHIPPED

**Shipped 2026-09-07.** `PlateSolveSettingsPanel.tsx` gained an **Input Gate**
section (toggle + both thresholds, the numbers disabled while the toggle is off)
and a **Batch Concurrency** field in Solver Parameters. Frontend-only, as
predicted below — no Rust, no new command, no migration. The owner's decision on
the open question: input gate plus `batch_concurrency`, and the acceptance-gate
group stays unexposed. What remains is the by-hand acceptance run, listed in
`docs/superpowers/open-items.md`.


The v0.5.5 release notes promised "the thresholds are in Settings if you want
them looser". They were not, and the sentence has been retracted from the notes,
the blog post and the download-page row. Owner decision the same day: expose
them for real.

**Why it matters.** The input gate refuses a frame whose own analysis shows
`median_eccentricity >= input_max_eccentricity` AND
`trail_r_squared >= input_min_trail_r2`. On a rig that trails a little — an
unguided mount, a windy site — the defaults may refuse frames the solver would
have handled. Today the only way to loosen them is to edit JSON in the settings
table by hand.

**Where everything already is:**

- `crates/athenaeum-core/src/plate_solve/config.rs` — `input_gate_enabled`
  (true), `input_max_eccentricity` (0.85), `input_min_trail_r2` (0.65), stored
  with the rest of `PlateSolveConfig` as one JSON blob under the settings key
  `plate_solve.config`.
- `get_plate_solve_config` / `set_plate_solve_config` /
  `reset_plate_solve_config` exist and are registered on **both** backends, and
  the panel writes the whole struct back on save.
- `src/components/plate-solve/PlateSolveSettingsPanel.tsx` renders exactly three
  fields — `base_verification_tolerance_arcsec`, `sip_order`,
  `autofind_tolerance_deg`. Its `DEFAULT_CONFIG` is a hand-written mirror of the
  whole struct (there is a comment saying so), which is why every other field
  round-trips through the panel untouched instead of being lost.

**So this is a frontend-only task**: no Rust change, no new command, no
migration. Three controls and their copy.

**Also unexposed, and part of the same decision:** the whole acceptance-gate
group — `blind_gate_enabled`, `blind_rms_max_px_mult`, `blind_min_inlier_ratio`,
`blind_inlier_floor`, `blind_scale_sanity_min` / `_max`,
`blind_scale_header_tol` — plus `batch_concurrency`.

**Open:** expose only the input gate, or the acceptance gate too? Proposed:
**only the input gate**, plus `batch_concurrency` if it is wanted. The
acceptance gate is what stopped v0.5.5's false 16–193× solutions from reaching
the catalog; a number a user can loosen without understanding it is a number
that will be loosened. If the acceptance gate ever does surface, it belongs
behind an "Advanced" disclosure with the failure it prevents named in the copy.

**Read this together with item 4.** The input gate reads the FULL analysis
path's eccentricity, which under-reports on exactly the frames the gate exists
for. Loosening the threshold is honest; tightening it will not catch what item 4
describes, and the copy should not imply that it will.

**Acceptance:** the toggle and both numbers persist across a restart and
round-trip through the panel; turning the gate off makes a frame it previously
refused attempt a solve again; the three fields already on the tab still save.

## 2. VNG debayer in the blink preview at full resolution

**Shipped 2026-09-06** (`b11cb0a9`, spec
`docs/superpowers/specs/2026-09-06-blink-full-resolution-vng-design.md`):
`Resolution::Full` now debayers CFA frames at native resolution with the
gradient method, one such render at a time, and the preview cache gained a byte
budget (`blink.memory_cache_max_mb`). The question that was open here was
settled as "always-on for `full`" (spec D1). What remains is the owner smoke
list in `docs/superpowers/open-items.md`.

## 3. Navigation memory ("backspace browsing")

Carried over from `docs/backlog-v0.5.5.md` item 1, minus the frame-card fields,
which shipped in v0.5.5. Every main screen should remember its last browsing
state; explicitly **not** pop-up modals and **not** blink. The calendar screen
resets entirely today.

**Open, unchanged:** is "back" the browser/history gesture, an in-app
affordance, or both? Does the state survive a full restart or only navigation
within a session? Scope is every page under `src/pages/`, so the answer decides
whether this is one shared hook or per-page work — the widest regression surface
of anything on this list.

## 4. The full analysis path under-reports eccentricity on trailed frames

Found while building the v0.5.5 plate-solve gates and deliberately left open
there, because the fix changes every stored metric.

The full detector measures shape over a stamp sized at `1.5 × field FWHM`. A
streak's bright head has a small FWHM, so the stamp is narrower than the object
and reports it rounder than it is — self-reinforcing. Measured on the owner's
real frames: **0.56 where the fast path, whose stamp follows the star's own size
(2 × HFD), sees 0.88.**

Consequences today: the Analysis table rates badly trailed frames well, and the
input gate of item 1 — which reads this number — misses them. Plate solving is
not fooled: it measures shape again per detection at its own scale, so such a
frame is refused rather than mis-solved.

**Open:** re-analysis story. Changing the stamp changes `median_eccentricity`
for every frame already in the catalog, so either every library needs a
re-analysis pass or the two measurements have to coexist for a while. That
decision, not the maths, is what makes this its own cycle.

## 5. One place to watch what the app is doing

Raised by the owner after the first in-app run of the integration-throughput
cycle (2026-09-06): the builds were clearly faster, but "progress is not
visible". Recorded so the idea is not lost; the owner's own verdict was that
the current state is acceptable for now.

What exists today is scattered per feature. The sidebar `ComputeQueueIndicator`
lists compute-queue jobs with a label and `running` / `queued` only — no stage,
no percent — because `ComputeQueueEntry` carries no subject id to join the
`master-build-progress` stream to (throughput spec §8). The only place a master
build's stage and percent render is the trailing cell of the calibration-table
row on the Coverage tab (`CreateMasterCell`, 10 px text), which is easy to miss
and absent from the Equipment page. Analysis has `AnalysisQueueIndicator`,
transfers have their own page, scan / export / archive / plate-solve each own a
widget. Nothing shows every running operation together.

Shape of the idea: one window or slide-over listing every in-flight operation —
master builds, analysis, plate-solve queue, exports, archive, transfers, content
index — each with stage, percent, bytes / ETA where its event stream carries
them, and a cancel. Master builds already emit `bytes_done` / `bytes_total` and
a `combining` stage; that per-set percent is also what the 2026-09-06 review
found non-monotonic (bytes scale for reading, rows scale for combining), which a
single panel would have to resolve rather than inherit.

**Open:** where it lives (a sidebar slide-over like the notification panel, or
a page); whether the smallest version — `ComputeQueueEntry` growing a subject
id so the existing sidebar card can show a percent — is enough on its own; and
whether the per-feature widgets fold into it or stay.

## 6. Camera identity from more than one header keyword — DROPPED

Raised by the owner 2026-09-07, analysed the same day, **dropped by the owner the
same day**: camera identity stays exactly as it is — one `INSTRUME` string, with
the path layouts keyed on it as they are now, and differences handled by hand.
Nothing to plan, nothing to build.

The measurement is kept below because it is a fact about the owner's real files,
not about a feature: it is the answer to "can we just read the camera id from the
header", and the answer is no. If the idea ever returns, start here rather than
re-measuring.

### What was measured (2026-09-07, the owner's dev catalog)

**39 084 frames, 39 084 stored headers** — 34 987 FITS-card blobs and 4 097 XISF
keyword dumps (the XISF blob is *not* XML; the scanner stores a `KEY = value`
text dump headed `XISF FITS Keywords:`). Every keyword in every header counted
and cross-tabulated with the writing software.

| keyword | headers | distinct values | what it actually is |
| ---- | ---- | ---- | ---- |
| `INSTRUME` | 39 075 | 10 | camera name — the only field present everywhere |
| `SWCREATE` | 27 479 | 12 | N.I.N.A. version |
| `CAMERAID` | 13 520 | 12 | **three different kinds of thing** — see below |
| `CREATOR` | 10 146 | 5 | ASIAIR firmware, written *instead of* `SWCREATE` |
| `GUIDECAM` | 4 663 | 4 | the **guide** camera — a trap for any keyword sweep |
| `READOUTM` | 9 624 | 3 | readout mode; config, not identity |
| `QHY_*` (9 keys) | 5 314 | — | exposure timing. No serial anywhere in them |
| `OBSERVER` | 1 048 | 2 | INDI/EKOS only |

**`CAMERAID` is the only candidate, and it cannot be trusted as written.**
Written by N.I.N.A. 3.x only — never 2.x, never ASIAIR — so coverage is 35 % and
is a function of software version, not of camera. Its content depends on the
driver:

- **QHY native** → `QHY268M-a137314fb15efe44a` — model plus a real serial.
- **ZWO native** → `ZWO ASI2600MC Duo` — a verbatim copy of `INSTRUME`. Zero
  information; nothing in any header distinguishes two identical ZWO cameras.
- **Touptek-family (`ATR2600M`)** → `\\?\usb#vid_0547&pid_1428#5&193dc1b4&0&9`
  — a **Windows USB device path**. One physical camera, **six distinct values**
  across 1 524 frames, because the path encodes the USB port and hub.

**`INSTRUME` is not stable across software either.** The same physical QHY268M
is `QHY268M` from N.I.N.A. (8 159 frames, with the `CAMERAID` above) and
`QHY CCD QHY268M-a137314` from INDI/EKOS (1 048 frames, no `CAMERAID`). The
shared token is the serial `a137314`, but INDI truncates it — the two never join
on equality, only on a prefix.

**`unique_camera` does not fit this layout.** It suffixes every frame under a
scan root, but the roots are storage volumes: root 3 holds 8 distinct `INSTRUME`
values, root 4 holds 8, root 5 holds 8. It is `0` on all nine roots.

### The one live consequence, and its manual repair

`instrume` is `Exact` by default on every source→calibration pair, so the two
QHY spellings cannot calibrate each other:

| identity | Light | Flat | Dark | Bias |
| ---- | ---- | ---- | ---- | ---- |
| `QHY268M` | 4 288 | 2 757 | 873 | 200 |
| `QHY CCD QHY268M-a137314` | 749 | 195 | 100 | **0** |

749 lights of that camera cannot reach the 200 bias frames of the same camera.
Consistent with the decision above, this is repaired by hand and needs no
feature: selecting those frames and setting `INSTRUME` to the other spelling in
the metadata pane (`bulk_update_frame_metadata`) sets `frames.override = 1`, so
the scanner will not undo it, and the matcher then sees one camera.

## 7. Unreadable and deleted files need a way out of the catalog — SHIPPED

**Shipped 2026-09-07.** `delete_missing_files` now calls
`relinking::delete_orphaned_files` on BOTH backends, pinned by a new route test,
so purging a master's file un-supersedes its raw set instead of stranding it.
The scan-error list gained a per-row reveal button that recovers the path from
the message.

**"Delete completely" for an unreadable file: DROPPED** (owner, 2026-09-07 —
"reveal is enough"). The original request named two buttons; reveal is the whole
answer, and neither an ignore list nor a disk deletion is wanted. Recorded so it
is not re-proposed as an oversight.

Measured while closing this: the owner's catalog has **zero** stranded supersede
pointers — 11 superseded raw sets, all pointing at masters whose files are on
disk; no master shell without member frames, no consumer link into one. The
defect was real and reproduced in a test, but it had never fired here, so there
is nothing to repair and no repair pass is needed.


Owner, 2026-09-07: a file that cannot be read, or that has been deleted, should
be removable from the database and not only relinkable; the read errors should
carry a button to reveal the file and one to delete it completely.

The request covers **two populations that live in completely different places**,
and only one of them has a database row at all.

**(a) Missing — was catalogued, then vanished from disk.** It has a
`missing_files` row, `get_missing_files` lists it, `MissingFilesPanel` inside
`MonitoredInspector` renders it, and deletion already exists:
`delete_missing_files` on both backends, wired to both a per-row and a bulk
button. Three things are wrong with it rather than absent:

- `delete_missing_files` runs a bare `DELETE FROM files WHERE id IN (…)`
  (`commands/missing_files.rs:251`). The other path for the same act,
  `relinking::delete_orphaned_files`, deliberately does more: it resolves
  `master_set_id_for_file` and un-supersedes the raw set (2026-08-02 audit C3),
  so raw frames don't stay invisible behind a master that exists neither on disk
  nor in the catalog. Delete a **master** through the Missing Files panel today
  and that repair never runs.
- `relocate_missing_file` is Tauri-only; the web route is a 501 stub
  (`routes/mod.rs:426`, "needs native file picker"). Defensible, but it means
  per-file relink is desktop-only.
- Missing-file actions are hidden while the root is offline (`showMissing =
  !offline && …`, folders spec §5.4) — deliberate, and it interacts with item 8.

**(b) Unreadable — present on disk, parse failed. There is no row.**
`scan_directory_parallel` catches the parse error, and even a panic, pushes a
formatted string `"{path}: {error}"` into `ScanResult.errors` and returns `None`
for that file (`scanner/mod.rs:1474`) — nothing is inserted anywhere. The list is
persisted per root as a JSON array of strings in `scan_roots.last_scan_errors`
and displayed by `MonitoredInspector` (the fallback chain
`scanResult?.errors ?? root.last_scan_errors`) and `ScanSummaryModal` (first
10, then "…and N more").
Consequences:

- "Delete completely" cannot mean "delete the catalog row" here — there is
  none. It can only mean deleting the file from disk, or an ignore list that
  stops it being retried.
- The file **is** re-read and re-fails on every scan, forever. Nothing dismisses
  it.
- Both buttons the owner asks for need the path as a field, and today it is
  spliced into the middle of a message string. So the first slice is structure:
  `errors: Vec<String>` becomes records (path, reason, kind), which changes
  `ScanResult`, the `scan-complete` event payload, `scan_roots.last_scan_errors`
  and the two components above. Everything else depends on that.

**Owner decisions, 2026-09-07.**

- **(a) is a defect, not a question.** `delete_missing_files` must go through
  `relinking::delete_orphaned_files` so a deleted master un-supersedes its raw
  set and repoints its consumers. Fixable on its own, ahead of the rest of the
  item.
- **(b) starts with reveal.** "At least add reveal by path — the path is shown
  in the error line anyway." It is: the message is built as `"{path}: {error}"`,
  so the path can be recovered by splitting on the first `": "` and a reveal
  button can ship without touching `ScanResult`. That is the first slice, and it
  is small. Structured error records stay the right end state — the split is a
  heuristic, and a path containing `": "` would defeat it — but they are no
  longer a prerequisite for the button the owner asked for.

**Why "delete completely" would not have been small, if it ever returns.** Every
Black Hole entry point is keyed on `file_id` and reads `files.path` from the row
(`commands/duplicates.rs::move_to_black_hole` / `bulk_move_to_black_hole` /
`send_to_void`, over `db/operations_blackhole.rs`, whose `black_hole` table has
`file_id` as its key). An unreadable file has no row, so none of them can be
pointed at it: the recoverable route would need a new path-keyed command on both
backends with its own `PathPolicy` check, and the alternative — a plain unlink —
is irreversible on a file the app admits it could not read well enough to show
the user what they are discarding.

## 8. An offline folder cannot be re-checked without touching another folder — SHIPPED

**Shipped 2026-09-07.** A **Check again** button on the offline banner of both
inspectors (`MonitoredInspector` and `RoleInspector` — a role folder hosts the
same banner and must not be left without a way back), backed by the shared
`RecheckButton` and `FoldersTab.handleRecheck`. Frontend-only, as predicted. No
background polling: the reason is in the component's own doc comment.


Owner, 2026-09-07: when a storage is offline the refresh button should be active
so the folder can be re-detected. Today the only way is to click rescan on a
*different* monitored directory — a workaround, not a solution.

Reproduced in the code exactly as described.

- Availability is one `Path::exists()` per root, computed only by
  `api::scan_roots::check_all_scan_roots_availability` (registered on both
  backends).
- The frontend calls it in three places, all inside
  `useScanRootsWithAvailability` (`hooks/useTauri.ts`): on mount, and after
  `relinkScanRoot`. `FoldersTab.refreshAll()` re-runs it — and `refreshAll` fires
  only from mutations: a scan of any root, add/remove, the monitor and duplicate
  toggles. **There is no timer and no explicit refresh control**; the Folders
  tab's only header button is "Add Folder".
- Which makes the owner's workaround the intended path by accident: the other
  root's scan calls `refreshAll`, which re-checks availability for *all* roots.
- And when a root is offline the whole action block is not rendered —
  `{!offline && (<Scan now/> <Relink…/>)}` in `MonitoredInspector.tsx` — while
  the offline banner offers exactly one button, "Relink — point to new
  location…". A drive that simply came back needs no relink: its path never
  changed, and the honest gesture for it does not exist in the UI.

**Fix shape: no backend work.** The command exists on both backends and returns
`Vec<(root_id, bool)>`. A "Check again" button on the offline banner (and/or one
in the Folders header) that re-runs it and, when the root is back, restores the
normal actions.

**Owner, 2026-09-07: "судя по всему легко решается" — agreed, and the code
above is the reason.** Ready to execute as written; no research left.

**Still open, and small:** button only, or automatic as well — on window focus,
on a timer, or on an OS mount event? `Path::exists()` against a dead network
mount can block for that mount's own timeout, so a background poll is a decision
to take deliberately rather than a default. And when a root does come back: mark
it available only, or start a scan? Neither answer blocks the button.
