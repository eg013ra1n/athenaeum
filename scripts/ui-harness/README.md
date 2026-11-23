# UI harness (dev only)

Renders the real collab project page on the approved mockup's sample data, so it
can be compared with `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
at the same viewport.

    npm run ui:harness                         # default data
    HARNESS_SCENARIO=long npm run ui:harness   # long names; also: filters, empty, offline, review, stacking

Open http://localhost:1430/projects/p-m31 at 1440 × 900 with the app sidebar
collapsed, and the mockup (served from any static server, "Design notes" off) at
the same size. Run `measure.js` on both pages and diff the JSON by text.
Tolerance: 1 px (spec §17.2). Nothing here ships in the app build.

The `review` scenario (`/projects/p-m31?tab=mine`) shows 48 calibrated frames in
To review, three withheld frames in Held back and a running calibrate run (12 of 48).
The harness serves commands over HTTP only (no events), so the run shows through the
`get_collab_publish_run` snapshot and never advances.

The `stacking` scenario (`/projects/p-m31?tab=stacking`) shows the project's Stacking tab on a plan shaped from
`src/types/stacking.ts`: an unconfirmed grid (a proposal the viewer may confirm) with the schematic of ten frame
outlines from three publishers, two of them reaching past the proposal, and two more frames without a WCS; the Prepare
board; three groups (`mono__L__st120`, `mono__Ha__st300`, `osc__None__st180`: a project key has no binning); and twelve frames in
the Frames table, one of them not on this device, one an unsupported file and two without a WCS, with their focal
lengths (FL). One finished run, #9, exists (`get_project_stacking_runs`, `get_stacking_run`): it wrote no master, and
its reference was moved by the second pass, so the reference strip shows every line and the schematic outlines that
frame in gold. The writes the tab can reach are echoed, never stored. The plan still
carries its `grid` blocker, so Run stays refused until a grid is confirmed.

The `mcp-plan` scenario (`HARNESS_SCENARIO=mcp-plan HARNESS_PLAN=black_hole|metadata|plate_solve npm run ui:harness`, default
`black_hole`) shows the assistant's plan window over any page: the web build asks `get_mcp_plan` on load and the harness answers
with a waiting plan. The Black Hole plan names the same files as the two groups on http://localhost:1430/files (Duplicates tab).
An answer is echoed (Apply ends in a report, Cancel in "cancelled"); reload the page to get the plan back. Fixtures: `mcpPlan.mjs`.

The `header-keywords` scenario (`HARNESS_SCENARIO=header-keywords npm run ui:harness`, then
http://localhost:1430/settings?tab=general&section=general.headerKeywords) shows Settings → General → FITS keywords on the
default chains, with a preview of 312 frames (290 sensor temperature, 22 exposure, 14 pixel size X), 9 values kept, 3 calibration
sets regrouped and 2 frames without a header, so the Apply dialog carries every line. `HARNESS_PREVIEW=empty` answers a preview with
nothing to change (the line is hidden). `set_header_fallbacks` echoes what it received (and logs the changed fields),
`reset_header_fallbacks` and `apply_header_fallbacks` answer like the real ones; nothing is stored, so a reload brings the
default chains back and the preview line stays after Apply. The harness has no events, so the `header-fallbacks-progress` line
(`Applying… 120 of 312 frames`) never shows here; the section's test drives it. Fixtures: `headerKeywords.mjs`.
Its Export… downloads a fixed JSON text; Import… (any file) answers by `HARNESS_IMPORT=change|same|refuse`: one changed field (Sensor temperature, the confirm lists it), no change (the dialog says the file matches), or a refusal (a warning notification with the server text). Import's Import button feeds the rules to the autosave; the desktop file dialogs cannot be shown here.
