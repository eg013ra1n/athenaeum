*Athenaeum v0.7.1: stack a collaboration project in the app, keep your archives as ZIP files, and see your frames aligned in Blink.*

This update arrives inside the app: click **Download and install** when it is offered.

## What's New

### Stack a collaboration project

- **Every project has a Stacking tab.** Stack all members' frames together on your own computer. The tab shows
  which frames are on this device, what the app will prepare before stacking, and where every member's frames lie
  on the sky.
- **One shared grid, so everyone's masters line up.** The coordinator confirms the grid every member stacks onto.
  The proposed grid turns with your reference frame, so the master lies the way your frames were shot, and the
  card shows how far it is turned from north.
- **A coverage map after each stack** shows where the project is deep and where it is thin.
- **Mix exposure lengths your way.** Group frames by exact exposure, within a stop, by equivalent exposure, or
  stack them all together.

### Choose the frames that go into a stack

- **A Frames table on the Stacking tab.** Group frames by stacking group, night, member or status, filter them,
  spot the outliers and exclude several frames at once.
- **Pick the reference frame with a star**, the same star as in Frame Analysis, and see which frame each run
  aligned to.
- **Blink your selection, aligned or original.** Select frames and press Blink: aligned frames stand still, so
  clouds, trails and satellites jump out at once. Switch to the originals any time. An aligned frame that stands
  taller than wide is turned so its long side is horizontal.

### Sharper, more reliable stacks

- **Sharper stars where the sharpest frames used to miss.** A few very sharp frames — most often when a set is
  aligned to a reference frame from another camera — could land a few pixels off and soften the stars in the
  stack. They now line up exactly. Frames that already aligned well are aligned exactly as before.
- **A frame that can't be aligned reliably no longer blurs your stack.** If a frame's alignment rests on only a
  small patch of stars, Athenaeum tries again from the frame's plate solution when it has one; if that does not
  hold either, the frame is left out of the stack.
- **Your best frames keep their star cores.** Rejection now matches frames by their stars' brightness, so the
  cores of your sharpest frames are no longer clipped. Your saved stacking settings switch to this once.
- **Weights follow how clear and how sharp a frame is.** Each frame's weight is measured on the stars every
  frame of the group shows, so a soft frame no longer gains weight from its blur, and clear, steady frames count
  most.

### Your ZIP archives are part of the catalog

- **Frames inside a ZIP file show up like any others.** A `.zip` in a scanned folder is read without unpacking:
  its frames appear in Objects with a **zip** chip, and they open in Blink, plate solve, stack, build masters and
  export just like frames on disk. Extracted copies are kept in a cache you can limit and clear in
  **Settings → General → Archive**.
- **Unpack from zip** puts a frame set back on disk for good, and **Unarchive** extracts only what is missing.
- **Look inside a ZIP in the file browser**, with each file's header in the metadata pane. A whole ZIP can be
  renamed, moved or sent to the Black Hole.
- **An unplugged drive never deletes anything:** its ZIP frames show as missing until the drive is back.

### A real sky under the Sky Chart

- **See the sky behind your targets.** DSS colour and six narrowband surveys of the northern sky — the Northern
  Sky Narrowband Survey by Stefan Ziegenbalg (RGB, Hα, [OIII], [SII] and two blends) — are drawn under the chart
  as you browse and kept on your disk.
- **Take it offline.** Download the whole sky ahead of time, with finer detail around your imaging locations and
  the Messier objects. You see an estimate first, and you can cancel and resume later.
- **Keeps drawing when a server is busy:** tiles come from every verified copy of a survey.
- **A size limit for the sky tiles,** 10 GB by default. **Settings → Sky Chart** shows how much space they take
  and clears them.

## Changes

- **The Sky Chart keeps its view** across pages, window resizes and restarts, and no longer slows down in a long
  session.
- **Black Hole: one delete button.** Select what you want gone and press **Send to Void**.
- **Object page:** Duplicates, Unpack and Move to Archive sit at the end of the Lights toolbar, and one refresh
  button beside the coordinates recomputes nights, sessions, centre and rotation.
- **Plate-solve batches** count the frames finished and name the file being solved.
- **Windows: link-mode export works without Developer Mode.** Where Windows refuses a link, the app makes a hard
  link, or a copy where that is impossible, and the completion card counts each.
- **About → Acknowledgements** credits the sky surveys.

## Bug Fixes

- **A frame stored twice is counted and stacked once.** Two copies of the same raw frame used to count twice in
  Objects and weigh twice in a stack; the second copy now reads "duplicate — not used".
- **Frame-set boxes on the Sky Chart point the right way.** They were mirrored, up to 27° off on some sets.
- **Emptying the Black Hole with a filter on** could also delete the entries the filter hid. Send to Void deletes
  only what you selected.
- **The plate-solve batch progress** no longer runs a frame ahead, and in the web version it no longer closes at
  once.

## Good to know after updating

- **The first stack after the update takes longer.** Each frame set re-measures its stars and re-aligns its
  frames once; calibration is kept. Later stacks run at normal speed.
- **Star numbers in a stack run read lower than before.** Compare frames within one run, not with older runs.
  Runs stacked by earlier versions show "SNR gain —" until you stack them again.

## Coming in 0.7.2

- **Ask an AI assistant to work with your catalog.** Athenaeum will connect to AI assistants that support MCP,
  such as Claude. Ask in plain words — "find my duplicate frames and plan what to send to the Black Hole", "fill
  in the missing metadata" — then review the plan and apply it. Off until you turn it on.
- **Frame and group weights from star photometry, measured against Gaia.** Athenaeum will compare the brightness
  of each frame's stars with their Gaia catalog magnitudes, so it knows how much light the frame really collected.
  A frame shot through haze or low in the sky gets the weight it deserves, and frames from different cameras,
  telescopes and nights in a project are compared fairly by their equivalent exposure.

## Coming in 0.7.3

- **Mosaics.** A mosaic target becomes one object in your library, panels and all, and collaboration projects can
  cover a mosaic, not just a single field.
- **Athenaeum's stacking engine as an open library.** The stacking, registration and local normalization that
  Athenaeum uses — already proven on real data — move into rustafits, our open-source image library, so anyone can
  build them into their own tools and projects, vibe-coded ones included.
