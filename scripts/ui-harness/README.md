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
