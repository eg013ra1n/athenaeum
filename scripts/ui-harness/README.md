# UI harness (dev only)

Renders the real collab project page on the approved mockup's sample data, so it
can be compared with `docs/superpowers/research/2026-09-29-collab-project-layout-mockup.html`
at the same viewport.

    npm run ui:harness                         # default data
    HARNESS_SCENARIO=long npm run ui:harness   # long names; also: filters, empty, offline

Open http://localhost:1430/projects/p-m31 at 1440 × 900 with the app sidebar
collapsed, and the mockup (served from any static server, "Design notes" off) at
the same size. Run `measure.js` on both pages and diff the JSON by text.
Tolerance: 1 px (spec §17.2). Nothing here ships in the app build.
