## What's New

- **Frame analysis pipeline** — star detection, FWHM, eccentricity, SNR, HFR, PSF signal, and quality scoring for light frames with parallel processing across all available CPU cores
- **Star annotations & metrics** — toggle annotated FITS image rendering in the blink viewer showing detected stars, annotation metrics, and a dedicated frame info panel
- **Trail detection** — automatic satellite/airplane trail detection with configurable R-squared threshold and visual warnings in the analysis table
- **Lights analysis table** — sortable table with real analysis data, rejection thresholds (FWHM, eccentricity, SNR weight, stars, score), and auto-selection of rejected frames
- **Stage / WIP / Archive tabs** — workflow pipeline on the Objects page to organize frame sets by lifecycle stage with count badges and tab-aware actions
- **Analysis settings** — configurable detection sigma, star area bounds, saturation fraction, max stars, and quality score weights in Settings

## Changes

- Calibration hierarchy queries now include object coordinates (OBJCTRA/OBJCTDEC) for light frames
- RA/Dec display falls back to converting numeric decimal degrees to HMS/DMS when FITS keywords are absent
- RA column sorting uses numeric comparison instead of string
- rustafits dependency switched from git fork to crates.io v0.5.5
- Default max eccentricity threshold updated to 0.9
- Stars detected count now uses post-filter count instead of raw detections

## Bug Fixes

- Fixed stars_detected to use filtered star count (stars.len()) instead of raw detection count
