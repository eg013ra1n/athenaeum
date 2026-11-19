# Athenaeum MCP — design (v1: duplicates, missing metadata, catalog questions)

Date: 2026-10-27; revised 2026-11-19 after the spike (§11). Status: draft for owner approval.

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

1. **Find duplicate frames** across the catalog or a scope (§6.6), using the app's existing duplicate detection, and
   propose a **Black Hole plan** the user approves.
2. **Find frames with missing metadata** (coordinates, object, camera, date, frame type — the categories the Missing
   Metadata view already has) across the catalog or a scope (§6.6: "ASI2600MM lights of 2024 without object and
   coordinates") and **fill them**: by inference from the frame's neighbours or the user's word, and coordinates by
   **plate solving**, each through a plan the user approves.
3. **Answer questions about the catalog** that change nothing — what was shot, how much, with what, how good, what is
   calibrated, where it is stored (§6.7) — in the chat, with no plan and no window (owner, 2026-11-19).
4. Answer in the language of the user's files: file names, folders and header values — never bare database ids.

## 4. Non-goals (v1)

- Manual calibration linking by a rule, preparing frames for a collab project, stacking, export. These are phase 2
  (§12), each with its own design.
- Changing settings, scan roots or special folders.
- Rewriting files on disk. Every v1 write is a catalog write or a move into the Black Hole.
- Remote access. The MCP is reachable only from the machine running the app, or through the web build's own server
  for the Docker build.
- A plan card in the chat or a form in the client (§6.4, §11). v1 confirms in Athenaeum's own window; a card may come
  later (§12).

## 5. Anti-goals — a failure even if it works

- **The MCP does something the UI would refuse.** It calls the same core paths with the same gates (path policy, plate
  solve input gate, the Black Hole's own checks). It never routes around a gate.
- **Permanent deletion.** `send_to_void` is never an MCP tool. Emptying the Black Hole stays a
  human action in the UI.
- **A silent apply.** Nothing changes without the user's own click in Athenaeum's window (§6.4), and an apply that
  changed less than the plan says so, item by item.
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
- **Both halves of a tool result read on their own.** Clients pass different halves to their model (spike,
  2026-11-19): Claude desktop's model reads only the text `content`, Claude Code's model reads only
  `structuredContent` (as JSON). Each half carries the file names, folders and header values the answer needs; neither
  is a bare list of handles.

### 6.2 Duplicates

- Detection is the app's: raw sub-frames by header fingerprint + size + name, masters and processed files by the full
  hash, optionally everything by the content hash (`api::files::get_duplicates`, the call the Duplicates page uses, which
  follows the `duplicates.use_content_hash` setting; `duplicates::find_duplicates` is dead code). The MCP adds no other
  definition of "same file".
- Each group says what matched (which of the three keys) and where each copy lives, with the folder's role. Roles from
  the catalog are facts: the scan-root kind (`normal`, `calibration_library`, `sync_incoming`, `collaboration`).
  Inside a `normal` root the catalog cannot tell a shooting folder from a processing folder, so the MCP
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
  frame types). The search runs over the whole catalog or a scope (§6.6), so "ASI2600MM lights of 2024 without object
  and coordinates" is one request (owner, 2026-11-19).
- **Inference** proposes a value with its evidence: frame type from the folder (`LIGHT`, `Flats`), date from the file
  name, camera from the frames around it ("40 frames in this folder have INSTRUME = …"). A proposal without evidence is
  not made. The write is `bulk_update_frame_metadata`: catalog only, sets `override = 1`, survives rescans.
- **The user's own statement is evidence too** (owner, 2026-11-19): "all frames in this folder are IC 5146" makes
  proposals whose evidence reads "by your instruction". They are still plan items the user ticks, and the instruction
  covers the frames it named, not later ones.
- **Plate solving** fills coordinates through the app's plate solve: the input gate applies and is reported per frame,
  accepted solves write RA/Dec and, where OBJECT is empty, the nearest object's name, exactly as the UI does.
  Before applying, the plan reports the eligible subset (`N of M`), frames the input gate will refuse and why, missing
  catalog layers or index packs, and the expected scale of the run.
- Actions apply only to the eligible subset of a mixed selection. The count is always shown (`N of M`).

### 6.4 Plan → choose → apply

Every change goes through a plan, and **only the user can apply it** (owner, 2026-10-27): the model prepares and
explains a plan, and no MCP tool applies one.

**The plan.** A planning tool returns a handle and the plan as data: groups (a duplicate group, a frame, a light group),
the items in each group with their file names, folders and header values, the options per group, and the totals. Two
kinds of option cover v1 and phase 2:

- **include / exclude** an item or a whole group (Black Hole moves, metadata fills, frames to solve);
- **choose one** among candidates (which copy of a group stays; in phase 2 which flat a light group gets, which frames go
  into a project).

The plan starts with the model's proposal pre-selected where the user gave an instruction, and with nothing selected
where there is none (§6.2: no keep rule). The plan cannot contain an item that breaks a rule (the last copy of a group,
a linked master); such items show as locked, with the reason.

**Where the user chooses and applies: Athenaeum's own window, in every client** (owner, 2026-11-19). No in-chat way
works in every client — Claude desktop draws an MCP Apps view and asks no forms, Claude Code asks forms and draws no
view, other clients vary (spike, §11) — so v1 has one interface, the one Athenaeum draws itself:

1. Making a plan opens it in the running app. The plan window (a `DialogShell` over the current page) shows the
   plan's build and catalog, its groups with file names, folders and header values, the options, the totals, and
   **Apply** / **Cancel**. On the desktop the app's window comes to the front (restored if minimised, shown if
   hidden); in the web build every open Athenaeum page shows it.
2. The user ticks and unticks items, picks one candidate per group, sees the totals update, and presses Apply or
   Cancel. Applying is the user's click in the app. **No MCP tool applies a plan**, so there is nothing for the model
   to call and nothing a client must hide.
3. The planning tool's answer tells the model that the plan is open in Athenaeum and waits for the user. The model
   learns the outcome — waiting, applied (with the report), cancelled, replaced, expired — through a status tool,
   never from its own claim. The assistant's "yes" in chat is never enough.
4. One plan is open in the window at a time. A new plan replaces one the user has not decided, and the replaced plan
   reports "replaced". With no window or page to show it (the web build with no page open), the plan still waits: the
   planning answer says "open Athenaeum to decide", and a page that opens shows the waiting plan until it expires.

The window is the app's own UI, so it may offer what the app already has for an item (reveal the file in the file
browser, open the frame's preview); v1 needs none of it.

**Apply.**

1. The selection must be a subset of the plan's options. Anything else is refused.
2. Apply re-checks every selected item against its state at planning time (path, size, modification time, the catalog
   values the plan read). An item that changed is skipped and reported, never applied on stale data.
3. The report lists applied, skipped and failed items, each with the reason; the window shows it in place and the
   status tool returns it.

Plans live in the app's memory, **app-wide, by handle** — not per connection: one client may open several connections
(Claude desktop opens two, measured), and the status tool may be asked on another one than the plan was made on.
Plans expire (the builder picks the lifetime). A plan from before a restart is gone.

### 6.5 Long operations

There is one plate-solve slot (`active_plate_solves` key 0; the queue lives in the frontend,
`src/hooks/usePlateSolveQueue.ts`). v1 keeps it: the MCP refuses to start a run while any batch runs, and says so; while
an MCP run is active the UI refuses to start a batch with a `notify()` naming the MCP run, and shows that a plate solve
started from the assistant is running with its progress. Moving the queue into core is not part of v1.
*(Builder-check call.)*

Plate solving runs as a background operation: the user's Apply starts it and the plan's status names its handle; a
status tool reports progress (frames done, the frame being solved by file name, failures so far) and the result; a
cancel tool stops it. The same operation shows in the app's UI as if started there, and its handle can be found in the
logs.

### 6.6 Scope

One scope, the same for every finding tool and every question (owner, 2026-11-19):

- Conditions: object (as in OBJECT, or a frame set's object), frame set, folder (with its subfolders), scan root,
  camera (INSTRUME), telescope (TELESCOP), filter, frame type, exposure, night, date range (a year is a range; a date
  is the night as the app counts it), and the frame's state: in the catalog, soft-archived, zipped, in the Black Hole.
  Conditions combine with AND; a value matches as the app's own filters match it.
- The answer restates the scope it used, in words ("ZWO ASI2600MM Pro, nights 2024-01-01 … 2024-12-31, lights"), so a
  misread scope shows at once.
- A scope that matches nothing says so and names the nearest values that exist ("no camera 'ASI2600'; the catalog has
  'ZWO ASI2600MM Pro' and 'ZWO ASI2600MC Duo'"); it never widens itself silently.

### 6.7 Catalog questions

Questions that change nothing are answered in the chat: no plan, no window. A question never writes the catalog, a
file or a setting, and never starts a long operation (a scan, an analysis, a plate solve).

- **What can be asked:** any fact the app already keeps and shows, over the whole catalog or a scope (§6.6), counted,
  totalled or listed:
  - what was shot — objects and frame sets, nights, frames by type, filters, exposures, cameras, telescopes, first and
    last night;
  - how much — frame counts and integration time, grouped by object, filter, camera, telescope, night, month or year;
  - how good — the measurements the app stores and shows (the Analysis tab, the frame panel: FWHM, star counts and
    the like, transparency loss where measured) as ranges, medians and outliers, with the frames named. A value that
    was not measured is said to be not measured, never estimated;
  - calibration — which light groups have darks, flats and bias linked, which have none, and the reason where the app
    states one (the export readiness sentences); no reason is invented where the app has none;
  - storage — files and bytes per folder and scan root, the archive state (WIP, soft-archived, zipped), the Black Hole;
  - missing metadata and duplicates as counts (§6.2 and §6.3 give the detail and the plans).
- **The numbers are the app's numbers.** The question tools read through the same core functions as the app's pages,
  so a figure in the chat equals the figure on the page for the same scope (integration time counted as the frame-set
  page counts it). No SQL, no query language, no table or column name in an argument or an answer — the reason this
  MCP exists (§1).
- **Few, coarse tools** (§11): a frame query (scope, grouping, totals, pages of files), the details of one file
  (header values, measurements, calibration links, where its copies live), a catalog overview. Answers are summaries
  with totals and a few example files, paged for the full list (§6.1).
- **A fact the catalog does not keep** ("which frames have satellite trails", a quality figure never measured) is
  said to be missing, not guessed. Where an app action would produce it, the answer names the action; where v1 has a
  plan for it (a plate solve, §6.3), the assistant may propose that plan.

## 7. Architecture (decided)

- **The MCP host lives inside the running app.** In the desktop build, the Tauri process. In the web/Docker build, the
  Axum server. It calls `athenaeum-core/src/api/` — the layer both hosts already call — and never opens the catalog
  from another process. Reasons: the app writes the catalog continuously (monitor, scanner, collab exchange); a second
  process would also start a second transfer node with the same device identity; the UI would not see the changes.
- **Desktop clients connect through a bridge.** A desktop MCP client starts local servers as programs (stdio). It
  cannot reach a port on `localhost` on its own. So the app ships a small bridge program, `athenaeum-mcp`. The client
  starts it; it forwards MCP messages to the running app over a local channel authenticated by a token. The bridge
  holds no catalog logic and never opens the database. If the app is not running, or the MCP is off, the bridge
  answers with that, in words. A plain byte pipe is enough: the spike carried every message through one, on macOS over
  a Unix socket (§11).
- **Two protocol revisions, many connections.** The host serves the `initialize` revision (2025-11-25; Claude desktop)
  and the stateless one (2026-07-28; Claude Code: `server/discover`, the client's capabilities in every request's
  `_meta`), and many connections at once (Claude desktop opens two). Plans and operations are app-wide (§6.4).
  Library: the official Rust SDK `rmcp` (3.11 at the spike) does both revisions and a stream transport.
- **Build selection.** The bridge is told which build to reach (release or dev). Each build keeps its own token and
  channel address in its own app-data folder, so the two cannot cross. The MCP server name the client sees is
  `athenaeum` or `athenaeum-dev`.
- **Web/Docker.** The web server exposes the MCP over HTTP. It always requires the MCP token from Settings (§8),
  whether or not `ATHENAEUM_API_KEY` is set (that key is opt-in and leaves `/api/*` open when unset), and the Settings
  toggle gates it exactly as on the desktop. A default Docker install therefore exposes nothing until the user turns the
  MCP on. The same core calls, no bridge. *(Builder-check call.)*
- **The UI stays current.** An MCP change emits the same events the UI's own action emits (for example
  `blackhole-changed`), so open views refresh.
- **Every answer starts with its build and catalog** (build name and version, catalog path). The plan window shows it in
  its header.
- **The plan window is part of the app's page tree** (§6.4): one generic plan view — groups, items, header columns,
  include/exclude, choose-one, totals, the report — so phase 2 reuses it without a new view per action. It reads the
  plan and sends the user's answer through the app's `api` object like any other page, and uses the app's theme and
  design tokens. The host tells the UI that a plan is waiting through the existing event channel (Tauri events on the
  desktop, SSE on the web).

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

- **Batch plate solving is already in core** (`api::plate_solve::prepare_plate_solve_batch` /
  `run_plate_solve_batch`; corrected 2026-11-19 by the plan's reuse audit). What each host still carries — the three
  resolvers for catalog layers, index packs and the DSO catalog, and the cancel body — moves into core, and core gains
  the busy query the one-slot rule needs (§6.5): today a second batch silently replaces the first one's cancel handle.
- **The Black Hole move has side effects in the wrapper.** `bulk_move_to_black_hole` (Tauri `commands/duplicates.rs`,
  and its Axum mirror) calls `collab_autopublish::note_black_hole_change` and emits `blackhole-changed` after the core
  call. The MCP must get exactly the same behaviour, so that part moves into core as well (the event through the
  host's emitter).
- **Two backends in sync.** The settings section's commands (enable, regenerate token, connection details) exist in
  both hosts, per the project rule.
- **Rebuilt-master-copies check** (§6.2) is new core code, with a test on a fixture library holding `_2` / `_3` copies.
- **Scope and question tools** (§6.6, §6.7) need one scope type in core that every finding and question tool takes,
  and reads over the existing core functions (frame-set totals, analysis values, calibration links, archive state).
  Where a page computes a figure in the frontend today, that computation moves into core so the page and the chat
  share it.
- **The plan window** (§6.4, §7) is new frontend work in the app: the generic plan view in a `DialogShell`, the event
  that a plan is waiting, and two commands both hosts register (read a plan, answer it), per the two-backends rule.
  On the desktop the host also brings the main window to the front.

## 11. Settled by the spike; left to the builder

**The spike (2026-11-19)** ran a throwaway server behind a plain stdio ↔ Unix-socket bridge against the real clients,
with every message logged. Evidence and sources: `docs/superpowers/research/2026-11-19-mcp-spike.md`.

| Question | Claude desktop 6.6395.1 (chat, agent mode) | Claude Code 2.1.328 (terminal) |
| ---- | ---- | ---- |
| Protocol revision | 2025-11-25 (`initialize`), two connections | 2026-07-28 (stateless) |
| Plan view (MCP Apps) | declared; drawn beside the chat; the view's own calls reach the host | not declared; no view |
| Form elicitation | not declared | declared; a form in the terminal (boolean, one choice, multi-select all answered) |
| Model sees an app-only tool | no; its attempt through the client's view-tool route failed, nothing reached the host | no ("no spike_apply tool") |
| Model reads | `content` only | `structuredContent` only |
| Result `_meta` | reached neither the model nor the view | did not reach the model |
| Through the dumb bridge | everything, the view included | everything |

**Decision (owner, 2026-11-19): one interface.** No in-chat way works in every client, so v1 confirms in Athenaeum's
window for all of them (§6.4) and builds neither the in-chat view nor the terminal form. What the spike measured about
them stays for a later plan card (§12). Still in force from it: both halves of a result read on their own (§6.1); two
protocol revisions and several connections per client (§7); a plain byte-pipe bridge (§7).

**Left to the builder:**

- The local channel on Windows (a named pipe or loopback with token; the Unix socket is measured on macOS), and how
  the bridge is packaged with each platform's installer.
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
- **A plan card in the chat** (MCP Apps) for clients that draw one, over the same app-wide plans, as an addition to
  the window once client support settles. The spike measured what it needs (§11,
  `docs/superpowers/research/2026-11-19-mcp-spike.md`): the card fetches its plan by handle through its own app-only
  call; the host lists and accepts app-only tools only on a connection that declared MCP Apps (the wire carries no
  mark of the caller, and a client that does not know MCP Apps would show them to its model); apply carries a one-time
  token the card got with the plan; the card registers no tools of its own (Claude desktop offers a model the tools a
  view registers); Claude desktop drops a result's `_meta` before the card and is reported not to draw it on a chat's
  first message.

## 13. Acceptance

On the owner's real catalogs, dev and release:

1. With the MCP off, the bridge answers that it is off; nothing else responds.
2. Duplicate groups from the MCP equal the Duplicates page's groups for the same setting.
3. A Black Hole plan from Claude desktop and from Claude Code: the plan opens in Athenaeum's window and the app comes
   to the front; unticking a group updates the totals; Cancel changes nothing; Apply moves exactly the ticked files,
   they show in the Black Hole tab, restore from there, and the open file views refresh without a reload.
4. The model cannot apply: no MCP tool applies a plan; asked to "apply it yourself", the model cannot, and its status
   tool reports "waiting for the user" until the click.
5. The web build with no page open: planning says to open Athenaeum, and a page that opens shows the waiting plan. A
   new plan replaces an undecided one, which reports "replaced"; an expired plan cannot be applied.
6. In Claude desktop the status tool answers for a plan made on its other connection.
7. An item changed on disk between plan and apply is skipped and reported.
8. The rebuilt-copies report finds the 2026-10-27 case: in `/Volumes/bigbase3/Calibration`, 108 masters, 81 distinct,
   27 extra copies (ATR2600M 12, ZWO_ASI2600MC_Duo 9, ZWO_ASI6200MM_Pro 6). A plan for them never contains a copy that
   calibration links point at.
9. Metadata filled by inference or by the user's instruction carries `override = 1` and survives a rescan; a proposal
   by instruction says so.
10. A plate-solve run started through the MCP gives the same result as the same frames solved from the UI; the UI shows
   it running and refuses its own batch meanwhile; the MCP refuses while a UI batch runs.
11. Dev and release both enabled at once: each answers for its own catalog and names itself.
12. Every MCP write is in the log with its plan.
13. Scope: the MCP's missing-metadata search for one camera and one year returns the same frames as the Missing
    Metadata view filtered the same way; a scope that matches nothing says so and names the nearest existing values.
14. Questions: frame counts and integration time for a frame set, per filter, equal its page; a quality range names
    its outlier frames by file; a question about a value never measured says so; no question changes the catalog
    (the question tools have no write path, and the frames in scope read the same before and after a session of
    questions — the file's modification time proves nothing, the app writes the catalog all the time).
