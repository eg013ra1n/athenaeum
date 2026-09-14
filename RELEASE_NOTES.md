*Athenaeum v0.6.3: a fix round for the stacking outputs — XISF masters that other applications open, a progress bar that moves frame by frame in every stage, a "Re-run from" menu that does what it says, and measurements that survive a cleanup.*

## Bug Fixes

- **XISF masters open in other applications.** A v0.6.2 XISF master was
  refused by another application with *"Missing bounds Image attribute,
  which is mandatory for a floating point real image"*. The XISF 1.0
  specification does require it for every floating-point image, and the
  writer had left it off. Every XISF file Athenaeum writes — master,
  drizzled master, weight map — now declares its representable range
  (`bounds="0:1"`, the range every master is built in) and its image type
  (`MasterLight` / `WeightMap`). A master written by v0.6.2 needs one
  **Re-run from › Integrate** — every cached stage is reused, only the
  integration and the header are redone; the pixels were never wrong.
- **"Re-run from" now lists what a run would reuse.** The menu used to
  offer the stages that were *already* stale — which every run redoes
  anyway — and refused to open when everything was cached, so no entry ever
  did anything ▶ Run did not. It now lists Calibrate, Measure, Register
  (and Local normalization when it is on) plus Integrate; each entry redoes
  that stage and everything after it and reuses the stages before. Stages
  with no usable cache stay visible but inert and say so, and the menu's
  footer — and ▶ Run's tooltip — state where the next run actually starts.
- **A cleanup that emptied the cache is explained.** After a run finished
  with *Output › cleanup: delete intermediates*, the next run silently
  started from Calibrate whichever stage it was re-run from. The plan now
  says so in one line naming the run and the policy ("Run #1 deleted its
  calibrated and registered frames afterwards … nothing is cached, so the
  next run starts from Calibrate"), and the *delete registered* case gets
  its own sentence.
- **Measurements survive "delete intermediates".** The cleanup used to
  discard every frame's measurement along with the calibrated files — a
  cached result that occupies no disk and costs a real set ten minutes to
  recompute. It is kept now (only *delete everything* removes it), so a
  re-run after that cleanup re-calibrates and re-registers but does not
  re-measure. Sets cleaned by v0.6.2 lost theirs already; the plan's line
  says whether measurements are still cached.
- **Progress moves frame by frame in every stage.** Measure, Register and
  Local normalization reported only when a whole group had finished — on a
  real set that is a bar frozen for minutes and then jumping by a group.
  Each now ticks per frame from inside the parallel workers, cached
  frames count from the start, the local-normalization reference build
  says what it is doing, and the Integrate row names the plane, the pass
  and the band it is reading beside its percentage and bytes. Every
  running stage row shows the same shape: count, percent, bytes when the
  stage reads them, the group, and the stage's own message.
- **Continuous integration is green again.** One test of the thin-plate-
  spline memory discipline assumed the number of resident displacement
  grids is bounded by the worker pool; it is bounded by the group's frame
  count during integration — one inverse grid per frame, by design, on any
  machine — and the test passed on a 10-core development machine only by
  coincidence, failing on every 4-core runner since v0.6.1. The bound now
  states the real invariant.

## Changes

- Stage rows during a run show `current / total · percent · bytes · group
  · message`, in the stage's own unit: builds, frames, frame-planes
  (Drizzle), groups (Output); Integrate shows no count — its plane, pass
  and band ride in the message.
- Web host: the folder picker's `browse_directories` answers `400 Bad
  Request` to an unknown `scope` instead of silently falling back to the
  scan roots (every shipped caller passes `scan`, `export` or `stacking`;
  landed with the Stacking tab in v0.6.0, unlisted until now).
