*Athenaeum v0.6.2: the stacking outputs round — Bayer drizzle for colour cameras, XISF masters, a catalog of every master light with a preview on its results card, and named presets you save yourself.*

## What's New

- **Bayer drizzle.** The Drizzle panel gains a *Bayer drizzle* switch for
  one-shot-colour groups. Calibration now keeps the calibrated CFA mosaic
  of every colour frame beside its debayered copy — one read, one
  calibration, two files — and drizzle deposits each sensor pixel into the
  colour plane it actually recorded, instead of drizzling the interpolated
  colour image. On the same two-camera set every earlier release was
  measured on, the drizzled colour master's green and blue stars are now
  within 0.5 % and 0.2 % of another application's CFA drizzle of the
  identical frames — where drizzling the debayered frames had left them
  10–12 % broader — and the colour deposit runs in about half the time.
  Colour-pure sampling also shows the colour offsets the night put into the
  data: red and blue stars sit a fraction of a pixel off green by exactly
  the amount that other application's CFA drizzle shows for the same
  frames, where debayering used to blend it away. Off by default; a
  re-run from Register re-uses the mosaics.
- **XISF output.** *Output format* in the Output panel: **FITS** (as
  before) or **XISF** — a monolithic XISF 1.0 file, one uncompressed
  32-bit float image, every master card carried as a FITS keyword, for the
  master, the drizzled master and its weight map alike (rejection maps stay
  FITS). Read back through Athenaeum's own reader, an XISF master matches
  the FITS master of the same run to every measured digit. The file states
  its row order explicitly; a set shot bottom-up produces one warning per
  group, because an XISF viewer will show that master mirrored against its
  FITS twin (the WCS stays correct for the stored array). Tested with
  Athenaeum's own reader — if another application opens one with the
  wrong levels, please report it.
- **Every master light is in the catalog.** A run records each file it
  writes — master, drizzled master, weight map — with its geometry, frame
  count and total exposure, per group, in a new `master_lights` table. It
  is the only place a master light is cataloged: it never becomes a frame,
  and the scanner's rule for calibrated artifacts is untouched.
- **Previews on the results cards.** The results card shows a thumbnail
  of each master and, when present, of its drizzled master — the same
  auto-stretched render the file browser uses, from FITS and from XISF,
  cached under the working folder and re-rendered when the master
  changes. The Working folder card counts the previews and the cleanup
  removes them.
- **Your own presets.** The Stacking tab's preset menu lists the three
  built-ins, a divider, and the presets you saved: **Save current as…**
  names the current configuration inline, a click applies one, the trash
  icon asks before deleting, and the toolbar label shows `'<name>'` when
  the configuration matches a saved preset. Presets never carry folders.
  The menu stays open during a run so you can save the settings you just
  launched with; only applying one waits for the run to finish. Stored in
  Settings, up to 50, names unique regardless of case.

## Changes

- Runs with *Bayer drizzle* off and *Output format* FITS produce
  byte-identical masters to v0.6.1 — no cached stage is invalidated by the
  new options, and turning Bayer drizzle on regenerates only the colour
  group's mosaics.
- A run's cached calibration is reported stale for a colour group whose
  mosaics the run would need and does not have yet, so the plan says what
  the first Bayer-drizzle run will redo.
- The master preview render takes the same image-processing permit the
  file browser's previews take, on desktop and on the web host, and a
  results card loads its drizzle thumbnail after its master's, so opening
  the results of a large run never reads several multi-hundred-megabyte
  masters at once.
- Rejection maps now carry the master's row order like the weight map
  does, so a FITS viewer overlays them the right way up.

## Bug Fixes

- None reported against v0.6.1 in this cycle.
