# Stacking Engine Roadmap — 2026-06-10

**Goal:** Path from the current registration foundation (`feature/stacking-prep`) to PixInsight-class image integration: sub-pixel-registered, normalized, rejection-combined stacks from calibrated full-resolution data.

## Where we actually stand (audit verdict)

**The geometric half is done and is the hard half.** Already shipping on this branch:

- Two-pass centroiding: intensity-weighted first pass, Moffat-2D LM refinement in precise mode (0.05–0.3 px), per-star σ recorded, SNR/FWHM fallback guards.
- Robust solve: quad matching → affine seed → PROSAC → iterative σ-clipped refit → annealed inverse-variance-weighted SIP (order 2–4 adaptive) → refinement-helped guard.
- Frame-to-reference registration with correct WCS composition (`CD' = CD_ref · M`, `CRPIX' = M⁻¹(CRPIX_ref − t)`), residual pairs + RMS (px and arcsec) persisted per frame (`registration_results`, user-chosen reference in `frame_set_reference`).
- E2E-tested below 0.5 px on real frames; all-f64 transform path.

**The missing half is the pixel pipeline.** rustafits today is render-oriented: full-res floats are transient, OSC debayer is half-res super-pixel (display) or green-only (analysis), output is stretched 8-bit. Nothing downstream of "transform computed" exists.

## Pre-work: registration guards (small, do before pixels — from the audit)

- [ ] Negative-determinant detection (meridian flip → mirror transform) — warn or auto-handle; today it silently produces a flipped mapping. (audit R1)
- [ ] Scale-consistency gate across a frame set (mixed binning composes a scale-wrong WCS). (audit R2)
- [ ] Parameterize `INLIER_TOL_PX` (fixed 4 px) by pixel scale. (audit R3)

## Phase A — calibrated linear pixel path (the enabler, biggest single piece)

Expose flux-linear full-resolution data from the image layer:

- rustafits API: decode FITS/XISF → full-res `f32` planes, **no stretch**, BZERO/BSCALE applied, optionally still-CFA (mosaiced) for late debayer.
- Master calibration application: bias/dark subtract, flat divide, using the calibration sets the matcher already links. Overflow/underflow policy (clamp vs NaN) decided once, documented.
- Quality debayer for OSC at full resolution (bilinear first; VNG/AHD later — super-pixel is not acceptable here).
- Memory model: stream per-frame (decode → calibrate → hand to consumer → drop); never hold N frames decoded.

## Phase B — reprojection

- Resample each calibrated frame into the reference frame's pixel grid using the stored affine + SIP.
- Kernels: bilinear (debug), Lanczos-3 (default), Lanczos-5 (option) with ringing clamp. Sub-pixel correctness test: synthetic star field, register, reproject, verify centroid shifts < 0.05 px.
- Pixel-center convention fixed and tested once (0-based everywhere; FITS 1-based only at WCS-card boundaries) — classic silent 1-px-shift trap.
- Output: registered frame + coverage/weight map (edges, flipped frames).

## Phase C — normalization

- Global: per-frame background (sigma-clipped median) offset + scale matching to reference. Required for any rejection to be meaningful.
- Later: local normalization (gradient-aware, tiled background model) — PixInsight's LN is a separate large feature; explicitly deferred.

## Phase D — integration

- Combiners: average, median, sigma-clip, Winsorized sigma-clip, linear-fit rejection. Weights: 1/noise² (from frame statistics), exposure, user.
- Memory strategy for 100+ subs: tile/chunk accumulation — first pass accumulates per-tile pixel stacks (or running statistics for clip-free modes), bounded by tile size × N, not full frames × N. Streaming two-pass for sigma-clip (pass 1: mean/σ per pixel; pass 2: clipped accumulate).
- Outputs: 32-bit float FITS + rejection maps. Progress via `ProgressEmitter`; runs on the existing global job-queue pattern (like analysis/plate-solve/registration).

## Phase E — deferred precision tail (pre-existing backlog, unchanged)

- Catalog v2 (f64 RA/Dec + per-star σ in the star catalog) and DCR correction — from `precise-registration.md` Phases 5–6. Improves absolute astrometry; **not** required for frame-to-frame stacking precision.
- Drizzle: only after Phases A–D are solid; needs the weight-map infrastructure from Phase B.

## Sequencing & effort shape

| Phase | Depends on | Rough shape |
| ---- | ---- | ---- |
| Guards | — | days |
| A: linear pixel path | — | the big one; touches rustafits API surface |
| B: reprojection | A + guards | medium; math is ready, kernel QA is the work |
| C: normalization | B | small–medium |
| D: integration | C | medium; memory strategy is the design risk |
| E: catalog v2 / DCR / drizzle | A–D | independent tail |

A minimal end-to-end "ugly but real" stack (bilinear + average, mono only) is reachable after A+B and is the right first milestone — it validates the full data path before kernel/rejection polish.
