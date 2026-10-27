# Athenaeum MCP — design (v1: duplicates and missing metadata)

Date: 2026-10-27. Status: draft for owner approval.

## 1. Intent

An MCP server lets an AI assistant answer questions about an Athenaeum catalog and carry out the catalog chores a
person now does by hand in the UI — in one conversation, over the whole catalog, faster than clicking.

The value is not faster clicking. It is the cross-cutting question the UI has no screen for ("find the extra XISF in
Cocoon, I want only the FITS originals, no copies"). Answering such a question today means raw SQL against the
catalog (25 queries in the 2026-10-27 session that found 27 rebuilt master copies and a nested Collaboration folder),
which is slow, guesses column names, and breaks with every schema change. The MCP gives the assistant a stable
vocabulary over the same `athenaeum-core` API the UI uses.

## 2. Who asks

All three, from v1 on:

| Who | Client | Build | MCP name |
| ---- | ---- | ---- | ---- |
| The owner as developer | an MCP client in the dev session (e.g. Claude Code) | dev (`com.vsharifov.athenaeum.dev`) | `athenaeum-dev` |
| The owner as astrophotographer | a desktop MCP client (e.g. Claude desktop) | release | `athenaeum` |
| Release users | their own MCP client | release | `athenaeum` |

The dev and release builds run on the same machine and deliberately share real folders (scan roots and special
roots). Collisions between them are wanted findings, not a misconfiguration. The MCP must never let an answer from
one build pass for the other.

## 3. Goals (v1)

1. **Find duplicate frames** across the catalog or a scope (object, folder, camera, night), using the app's existing
   duplicate detection, and propose a **Black Hole plan** the user approves.
2. **Find frames with missing metadata** (coordinates, object, camera, date, frame type — the categories the Missing
   Metadata view already has) and **fill them**: by inference from the frame's neighbours, and coordinates by
   **plate solving**, each through a plan the user approves.
3. Answer in the language of the user's files: file names, folders and header values — never bare database ids.

## 4. Non-goals (v1)

- Manual calibration linking by a rule, preparing frames for a collab project, stacking, export. These are phase 2
  (§12), each with its own design.
- Changing settings, scan roots or special folders.
- Rewriting files on disk. Every v1 write is a catalog write or a move into the Black Hole.
- Remote access. The MCP is reachable only from the machine running the app, or through the web build's own server
  for the Docker build.

## 5. Anti-goals — a failure even if it works

- **The MCP does something the UI would refuse.** It calls the same core paths with the same gates (path policy, plate
  solve input gate, the Black Hole's own checks). It never routes around a gate.
- **Permanent deletion.** `send_to_void` is never an MCP tool. Emptying the Black Hole stays a
  human action in the UI.
- **A silent apply.** Nothing changes without the user's own click or answer in a channel the model cannot speak for
  (§6.4), and an apply that changed less than the plan says so, item by item.
- **The user approves the model's retelling.** Choices are made on Athenaeum's own rendering of the plan, never on a
  table the model typed.
- **Ids instead of files.** An answer a person cannot act on without a lookup.
- **One build answering as the other.** Every answer names its build and catalog.
- **On by default.** The MCP is off until the user turns it on in Settings.

## 6. Decided behaviour

### 6.1 Answer language

- A frame is named by file name and folder, then the header values that describe it: IMAGETYP, OBJECT, FILTER,
  EXPTIME, GAIN/OFFSET, CCD temperature, binning, frame size, DATE-OBS (night), INSTRUME, TELESCOP.
- A group reads like "IC 5146, night 2024-10-05, filter H, 300 s, 24 frames".
- Dates use the app's one format, `YYYY-MM-DD HH:MM:SS` (see the date-format rule).
- Database ids appear only as handles the assistant passes back (a plan item, a follow-up question). They are never
  the description.
- Large results come as summaries — counts per group plus a few example file names — with pagination for the full
  list. A 40k-file catalog never comes back as a dump.
- No table names, column names, hashes or calibration-set ids in the description.

### 6.2 Duplicates

- Detection is the app's: raw sub-frames by header fingerprint + size + name, masters and processed files by the full
  hash, optionally everything by the content hash (`api::files::get_duplicates`, the call the Duplicates page uses, which
  follows the `duplicates.use_content_hash` setting; `duplicates::find_duplicates` is dead code). The MCP adds no other
  definition of "same file".
- Each group says what matched (which of the three keys) and where each copy lives, with the folder's role. Roles from
  the catalog are facts: the scan-root kind (`normal`, `calibration_library`, `sync_incoming`, `collaboration`,
  `master_stash`). Inside a `normal` root the catalog cannot tell a shooting folder from a processing folder, so the MCP
  states that split as an inference with its evidence ("this folder holds MasterLight XISF and a processing project,
  looks like a processing folder"), never as a fact. *(Builder-check call: no new data, no root labelling in v1.)*
- **Raw sub-frames matched by header fingerprint are byte-verified before they enter a Black Hole plan.** Only the
  copies the user's instruction would move are compared with the copy that stays (`verify_duplicate_pair`), so the cost
  is bounded by the plan, not the catalog; a pair that differs in bytes is dropped from the plan and reported.
  Planning a large plan is therefore a long, cancellable operation (§6.5). *(Builder-check call.)*
- Separately from byte duplicates, the MCP reports **rebuilt copies of one master**: masters in the calibration
  library with identical parameters (camera, type, exposure, temperature, gain, offset, binning, frame size, filter,
  shooting date), typically the `_2` / `_3` collision suffix. They are not duplicates by any hash. The report says
  which copy calibration links point at. (Builder's note: this check does not exist yet; it goes into core so the UI
  can show it later.)
- **Rebuilt copies can go into a Black Hole plan** (owner, 2026-10-27). The copies of one master form one group. A copy
  that calibration links point at always stays; only unlinked copies may be planned, on the user's instruction, and at
  least one copy of the group stays. The same keep-instruction rules apply as for byte duplicates.
- **There is no keep rule.** The user says, case by case, which copy stays ("in these groups keep the bigbase copy").
  The MCP may lay out its observations (shooting folder vs processing vs Sync), but it never picks on its own. A group
  without an instruction stays untouched. An instruction applies to the groups it was given for, not to future ones.
- At least one copy of every group stays where it is, whatever the instruction says.
- A master that calibration links point at is never put in a Black Hole plan in v1 (moving it would unlink lights).
  The plan lists it as kept, with the reason.

### 6.3 Missing metadata

- Categories and their scope are the Missing Metadata view's (`db::get_frames_with_missing_metadata`): coordinates
  (lights only, including the 0/0 and `00:00:00` placeholders), object (lights only), camera, date, frame type (all
  frame types).
- **Inference** proposes a value with its evidence: frame type from the folder (`LIGHT`, `Flats`), date from the file
  name, camera from the frames around it ("40 frames in this folder have INSTRUME = …"). A proposal without evidence is
  not made. The write is `bulk_update_frame_metadata`: catalog only, sets `override = 1`, survives rescans.
- **Plate solving** fills coordinates through the app's plate solve: the input gate applies and is reported per frame,
  accepted solves write RA/Dec and, where OBJECT is empty, the nearest object's name, exactly as the UI does.
  Before applying, the plan reports the eligible subset (`N of M`), frames the input gate will refuse and why, missing
  catalog layers or index packs, and the expected scale of the run.
- Actions apply only to the eligible subset of a mixed selection. The count is always shown (`N of M`).

### 6.4 Plan → choose → apply

Every change goes through a plan, and **only the user can apply it** (owner, 2026-10-27): the model prepares and
explains a plan, but no tool the model can call applies one on its own.

**The plan.** A planning tool returns a handle and the plan as data: groups (a duplicate group, a frame, a light group),
the items in each group with their file names, folders and header values, the options per group, and the totals. Two
kinds of option cover v1 and phase 2:

- **include / exclude** an item or a whole group (Black Hole moves, metadata fills, frames to solve);
- **choose one** among candidates (which copy of a group stays; in phase 2 which flat a light group gets, which frames go
  into a project).

The plan starts with the model's proposal pre-selected where the user gave an instruction, and with nothing selected
where there is none (§6.2: no keep rule). The plan cannot contain an item that breaks a rule (the last copy of a group,
a linked master); such items show as locked, with the reason.

**Where the user chooses and applies — three channels, in order.** The host picks the first one the connected client
supports, from the capabilities the client declares when it connects:

1. **The plan view in the chat (MCP App).** The planning tool's result carries Athenaeum's own interactive view, which
   the client renders in the conversation. It is HTML that Athenaeum ships (§7) and draws from the plan data, so what
   the user sees is exactly what will be applied, never the model's retelling. The user ticks and unticks items, picks
   one candidate per group, sees the totals update, and presses **Apply** or **Cancel**. Apply is a tool visible to the
   view only (`_meta.ui.visibility: ["app"]`): the client hides it from the model and refuses it to the model. The
   view sends the user's selection with it.
2. **A question from the server (elicitation).** For a client without MCP Apps that supports form elicitation (for
   example a terminal client), apply is a model-visible tool that does nothing by itself: the server asks the user
   through the client — the plan's totals, then accept / decline, and, when the plan has few groups, a multi-select of
   the groups to exclude. The answer goes from the user to the server, not through the model. A plan with
   choose-one options or too many groups for a form cannot be decided here; the server says so and points to channel 1
   or 3.
3. **A window in Athenaeum** (last resort). For a client with neither: apply opens a confirmation in the running app (in
   the web build, in its page) with the same plan view and Apply / Cancel. With no app window or page open, apply
   refuses and says why.

In every channel the MCP call that applies returns "applied", "cancelled by the user", or "no answer" after a timeout
(the builder picks it), and the model can read the plan's state through a status tool. The assistant's "yes" in chat is
never enough on its own.

**Apply.**

1. The selection must be a subset of the plan's options. Anything else is refused.
2. Apply re-checks every selected item against its state at planning time (path, size, modification time, the catalog
   values the plan read). An item that changed is skipped and reported, never applied on stale data.
3. The report lists applied, skipped and failed items, each with the reason, and the view shows it in place.

Plans live in the app's memory and expire (the builder picks the lifetime). A plan from before a restart is gone. One
plan can be waiting for an answer per client at a time.

### 6.5 Long operations

There is one plate-solve slot (`active_plate_solves` key 0; the queue lives in the frontend,
`src/hooks/usePlateSolveQueue.ts`). v1 keeps it: the MCP refuses to start a run while any batch runs, and says so; while
an MCP run is active the UI refuses to start a batch with a `notify()` naming the MCP run, and shows that a plate solve
started from the assistant is running with its progress. Moving the queue into core is not part of v1.
*(Builder-check call.)*

Plate solving runs as a background operation: apply returns an operation handle; a status tool reports progress
(frames done, the frame being solved by file name, failures so far) and the result; a cancel tool stops it. The same
operation shows in the app's UI as if started there, and its handle can be found in the logs.

## 7. Architecture (decided)

- **The MCP host lives inside the running app.** In the desktop build, the Tauri process. In the web/Docker build, the
  Axum server. It calls `athenaeum-core/src/api/` — the layer both hosts already call — and never opens the catalog
  from another process. Reasons: the app writes the catalog continuously (monitor, scanner, collab exchange); a second
  process would also start a second transfer node with the same device identity; the UI would not see the changes.
- **Desktop clients connect through a bridge.** A desktop MCP client starts local servers as programs (stdio). It
  cannot reach a port on `localhost` on its own. So the app ships a small bridge program, `athenaeum-mcp`. The client
  starts it; it forwards MCP messages to the running app over a local channel authenticated by a token. The bridge
  holds no catalog logic and never opens the database. If the app is not running, or the MCP is off, the bridge
  answers with that, in words.
- **Build selection.** The bridge is told which build to reach (release or dev). Each build keeps its own token and
  channel address in its own app-data folder, so the two cannot cross. The MCP server name the client sees is
  `athenaeum` or `athenaeum-dev`.
- **Web/Docker.** The web server exposes the MCP over HTTP. It always requires the MCP token from Settings (§8),
  whether or not `ATHENAEUM_API_KEY` is set (that key is opt-in and leaves `/api/*` open when unset), and the Settings
  toggle gates it exactly as on the desktop. A default Docker install therefore exposes nothing until the user turns the
  MCP on. The same core calls, no bridge. *(Builder-check call.)*
- **The UI stays current.** An MCP change emits the same events the UI's own action emits (for example
  `blackhole-changed`), so open views refresh.
- **Every answer starts with its build and catalog** (build name and version, catalog path). The plan view shows it in
  its header.
- **The plan view is one MCP App resource** (`ui://athenaeum/plan` or similar) that the MCP host serves. It is a single
  self-contained HTML bundle built from this repository's frontend (React, Tailwind, the shared `src/components/ui`
  primitives where they fit), with no network access of its own and no `@tauri-apps/*` imports. It renders any plan
  (§6.4) generically — groups, items, header columns, include/exclude, choose-one, totals, the report — so phase 2
  reuses it without a new view per action. It follows the client's light/dark theme. It talks only to the MCP host,
  through the MCP Apps bridge, never through the app's `api` object.

## 8. Settings

- A new Settings section (name left to the builder; for example "Assistant access (MCP)"), **off by default**.
- When on, the section shows how to connect: the bridge path and a ready-to-copy client configuration for this build,
  including the build's MCP name.
- The token can be regenerated, which cuts off every connected client.
- The section follows the Settings contract (`docs/settings/README.md`): registry entry, no Save button, no banner,
  `notify()` only on failure.
- Turning it off stops the MCP host; a connected bridge then answers "the MCP is off in Settings".

## 9. Logging and audit

- Every MCP tool call is a command boundary and wears `#[tracing::instrument(skip_all, err)]`.
- Every write records that it came through the MCP and the plan it applied. This needs new field names (for example
  `via = "mcp"`, `plan_id`, `tool`); per the logging rules they go through the logging spec's field dictionary first.
- The log MCP (`athenaeum-logs`) reads the dev and release builds' log folders together without saying which is
  which. v1 does not change that. The `athenaeum` MCP's own answers always name their build.

## 10. Groundwork the builder will meet

- **Batch plate solving is orchestrated in the host wrappers, twice.** `plate_solve_batch` in
  `crates/athenaeum-tauri/src/commands/plate_solve.rs` and its mirror in `crates/athenaeum-web/src/routes/plate_solve.rs`
  each carry the loading, input gate, worker and result-writing logic. Lift it into `athenaeum-core` so the UI and
  the MCP run one path; both wrappers become thin.
- **The Black Hole move has side effects in the wrapper.** `bulk_move_to_black_hole` (Tauri `commands/duplicates.rs`,
  and its Axum mirror) calls `collab_autopublish::note_black_hole_change` and emits `blackhole-changed` after the core
  call. The MCP must get exactly the same behaviour, so that part moves into core as well (the event through the
  host's emitter).
- **Two backends in sync.** The settings section's commands (enable, regenerate token, connection details) exist in
  both hosts, per the project rule.
- **Rebuilt-master-copies check** (§6.2) is new core code, with a test on a fixture library holding `_2` / `_3` copies.
- **The plan view** (§6.4, §7) is new frontend work: a second Vite entry that builds the self-contained HTML bundle, and
  the MCP host serving it as a resource. It is outside the app's page tree, so the app's `api`-object rule does not
  apply to it; the rule that nothing outside `src/api/` imports `@tauri-apps/*` still does.
- **The Athenaeum-window fallback** (§6.4 channel 3) reuses the same plan view inside a `DialogShell` modal in the app:
  the host sends the plan to the UI through the existing event channel (Tauri events on the desktop, SSE on the web),
  and the user's answer comes back through a command that both hosts register.

## 11. Left to the builder

- **First, a spike** (throwaway) that settles what the plan rests on, before the implementation plan pins anything:
  - which of channel 1 (MCP Apps view, app-only tools) and channel 2 (form elicitation) the current Claude desktop and
    Claude Code actually support, and how each shows them;
  - whether the client really refuses an app-only tool to the model, and whether any data can reach the view without
    passing through the model. If neither holds, app-only apply rests on the client's enforcement alone; the design
    then needs a server-side check (for example a one-time token only the view receives), and the owner hears about
    it before the plan;
  - whether the plan view can be served and rendered through the stdio bridge as it is.

- The MCP library for Rust (verify the current state of the official SDK before the plan pins it), the local channel
  (Unix socket / named pipe or loopback with token), and how the bridge is packaged with each platform's installer.
- Exact tool names and their argument shapes, within §6. A small, coarse tool set beats mirroring commands.
- Plan lifetime, page sizes, summary sizes.
- The Settings section's name and wording.

## 12. Phase 2 (sketch, separate design)

- Manual calibration linking by a rule stated in conversation: candidates via
  `get_calibration_sets_for_manual_selection`, a plan "group → chosen master → why → rejected candidates and why",
  shown in the same plan view as a choose-one per light group, apply via `manual_assign_calibration`. Frame size stays a hard rule; a sub-frame-origin mismatch warns. A rule meant
  to last belongs in the calibration matching config, not in the conversation.
- Preparing frames for a project: selection, quality screening, the shared readiness gate (`check_mode_ready`),
  withholding frames, publishing or exporting.

## 13. Acceptance

On the owner's real catalogs, dev and release:

1. With the MCP off, the bridge answers that it is off; nothing else responds.
2. Duplicate groups from the MCP equal the Duplicates page's groups for the same setting.
3. A Black Hole plan, in a client with MCP Apps: the plan view appears in the chat; unticking a group updates the
   totals; Cancel changes nothing; Apply moves exactly the ticked files, they show in the Black Hole tab, restore from
   there, and the open file views refresh without a reload.
4. The model cannot apply: in a client with MCP Apps, apply is not among the model's tools; asked to "apply it
   yourself", the model cannot.
5. In a client without MCP Apps but with elicitation, the same plan is applied through the server's question; with
   neither, through the Athenaeum window; with neither and no app window open, apply refuses and says why.
6. An item changed on disk between plan and apply is skipped and reported.
7. The rebuilt-copies report finds the 2026-10-27 case: in `/Volumes/bigbase3/Calibration`, 108 masters, 81 distinct,
   27 extra copies (ATR2600M 12, ZWO_ASI2600MC_Duo 9, ZWO_ASI6200MM_Pro 6). A plan for them never contains a copy that
   calibration links point at.
8. Metadata filled by inference carries `override = 1` and survives a rescan.
9. A plate-solve run started through the MCP gives the same result as the same frames solved from the UI; the UI shows
   it running and refuses its own batch meanwhile; the MCP refuses while a UI batch runs.
10. Dev and release both enabled at once: each answers for its own catalog and names itself.
11. Every MCP write is in the log with its plan.
