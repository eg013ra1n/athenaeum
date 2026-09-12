*Athenaeum v0.6.1: the stacking algorithms round — four more outlier-rejection methods, large-scale rejection for satellite trails, thin-plate-spline registration, a local scale for local normalization — and the Docker image is back.*

## What's New

- **Four more rejection methods.** The Integrate panel now offers
  **ESD** (the generalized extreme studentized deviate test, with its
  outliers fraction, α and low relaxation), **RCR** (robust Chauvenet
  rejection), **min/max** (drop the n lowest and highest samples) and the
  existing **Winsorized sigma clipping** rebuilt as the reference's
  iterated location/scale loop. Auto still picks the linear fit for large
  stacks and Winsorized for medium ones — nothing changes unless you
  choose a method. On a two-camera set every new method rejected fewer
  samples than the linear fit and produced a 6–13 % quieter master; RCR
  and Winsorized at a tight high sigma clip the cores of faint stars on
  colour data, so prefer the linear fit there (the help text says so).
- **Large-scale rejection.** A second integration pass that turns the
  per-pixel rejection map into connected structures — a satellite trail,
  an aircraft, a cosmic-ray shower — grows them, and rejects them whole,
  so a trail's faint shoulders go with its bright core. Off by default;
  costs about three times the integration time.
- **Thin-plate-spline registration.** The Register panel's distortion
  model gains **tps**: a regularized thin-plate spline over the alignment
  inliers, with a smoothing control (default 0.5) and an optional local
  distortion loop that re-pairs stars where the field disagrees with the
  linear model. Its reported RMS is a hold-out number — measured on stars
  the spline did not fit — so it is honest, not zero.
- **Local scale for local normalization.** The Normalize panel's *Local
  scale* checkbox is live: the per-frame scale becomes a smooth surface
  fitted over the matched stars, for flat-field residuals a single number
  cannot follow. Off by default. The star matching also gained a second
  pass on star barycentres for frames whose fits walked.
- **Structure-map seed detector.** The Measure panel can detect star
  seeds on a multiscale structure map instead of the plane
  (*Seed detector: structure*) — for undersampled colour frames on a
  sharp night. Default stays *peak*.

## Changes

- **Every Winsorized master differs slightly from its pre-0.6.1 self.**
  The Winsorized sigma clipping used for medium-sized calibration stacks
  now iterates the reference's location/scale loop; on a 100-frame master
  dark rebuilt both ways the median moved 0.002 %, the MAD 0.01 % and the
  hot-pixel count 0.9 %. Rebuild a master if you want it on the new loop;
  nothing forces you to.
- A `tps` registration releases its displacement grids stage by stage, so
  a 368-frame run stays within memory; a colour frame's grid is rebuilt
  once per plane in drizzle.
- The Docker image publishes again: the build's dependency-caching stage
  stubs the core crate's example targets, which had failed `cargo fetch`
  since v0.5.6.

## Bug Fixes

- The stacking run thread now tears down its context before it
  de-registers, so a client that waits for a run's completion never sees
  a run still dropping its state.
