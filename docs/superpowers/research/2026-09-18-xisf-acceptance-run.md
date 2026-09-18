# XISF acceptance run — calibration masters, calibrated lights, a mixed-container stack (2026-09-18)

Plan: `docs/superpowers/plans/2026-09-18-xisf-output-masters-and-calibrated-lights.md`,
Task 7. Code under test: `main` at `80b643e7` (Tasks 1–6), served as a release
`athenaeum-web` from `.superpowers/target-acc` on port 8934 against a
`.backup` copy of the dev catalog in `/Volumes/BigMac/Users/astrobureau/acc-xisf/`
(library, export and stacking folders redirected there — the owner's real
library was never written). Harness:
`docs/superpowers/research/scripts/acceptance/` (`prepare-catalog.sh`,
`server.sh`, `api.sh`, `imgcmp.py`, and `xisf-acceptance-run.sh`, the call
sequence of this run). Every pixel comparison below is `imgcmp.py`: float32
FITS or XISF read raw, `|a − b|` over every pixel.

## 1. Data

- **Lights:** the 20 sharpest mono LDN 1272 frames (ATR2600M, 180 s,
  6224×4168, by run 36's `fwhmPx`), copied out of the catalog twice —
  `pure/` (20 FITS) and `mixed/` (10 FITS + the other 10 rewritten as XISF by
  `examples/xisf_convert_probe.rs`: Float32, `bounds="0:65535"`, the source's
  45 cards). Cataloged as two custom sets (205 pure, 204 mixed) through the
  scanner: 10 of 20 `files.format = XISF` in the mixed set, all typed Light.
- **Masters:** the raw dark set 1679 (80 × 180 s, Touptek 2600M, the source of
  the catalog's FITS master dark 1749) un-superseded in the copy and built
  TWICE by the same engine — as XISF (set 1810, `calibration.master_format =
  xisf`) and then as FITS (set 1811). Master flat 1753 (FITS, the catalog's)
  for every light. The dark is a manual link: the library master's `OFFSET`
  is 200 against the lights' 30, exactly as the original LDN 1272 links are
  manual overrides.

## 2. The XISF master (Task 7 step 1)

`library/ATR2600M/MasterDark/master_dark_180s_-10C_g100_bin1x1_2025-04-12.xisf`,
103,770,624 bytes, signature `XISF0100`:

| Attribute / card | Value |
| ---- | ---- |
| `geometry` / `sampleFormat` | `6224:4168:1` / `Float32` |
| `bounds` | `0:65535` |
| `imageType` | `MasterDark` |
| `IMAGETYP` / `EXPTIME` / `CCD-TEMP` | `'Master Dark'` / `180.0` / `-9.9175` |
| `GAIN` / `OFFSET` / `XBINNING` / `INSTRUME` / `ROWORDER` | `100` / `200` / `1` / `'ATR2600M'` / `'TOP-DOWN'` |
| `XISF:CreatorApplication` | `Athenaeum 0.6.4` |

Catalog: `files.format = XISF`, `frames.imagetyp = MasterDark`,
`frames.is_master = 1`, `calibration_set` 1810 `is_master_library = 1`,
`master_provenance` 1810 ← 1679, `calibration_set.superseded_by_set_id` on
1679 = 1810 — the same rows a scanned XISF master gets (the
`direct_registration_matches_scanner_ingestion_xisf` pin, now on real data).

**Master vs its FITS twin (1811, same engine, same recipe `Average |
Winsorized sigma (3.0/3.0)`):** median 201.308 ADU both, `max |diff| = 0`,
0.0000 % of pixels differ. The container changes nothing in the pixels.

## 3. Calibrated lights (steps 1 and 3)

Four exports of set 205 in `calibratedLights` mode (flat-norm on, hot-pixel
on, debayer on — mono, so no debayer), 20 frames each, no warnings:

| Export | Dark master | Output | Compared with | Result |
| ---- | ---- | ---- | ---- | ---- |
| A | 1749 (the catalog's FITS master, built before M4c) | FITS | B | median 0, p99 6.4e-6, max 0.6, 16.16 % of pixels differ — **not the container**: 1749 and 1810 are different builds (M4c changed Winsorized), so their hot-pixel maps and pixels differ. Kept as the reminder to always control the engine, not just the file. |
| B | 1810 (XISF) | FITS | D | **`max |diff| = 0` on all 20 frames** |
| C | 1810 (XISF) | XISF | B | **`max |diff| = 0` on the 4 frames checked**; header `bounds="0:1"`, `imageType="Light"`, `IMAGETYP 'Light Frame'`, `CALSTAT 'BDF'`, `ATH_CSCL 65535.0`, `ATH_CHPX 104078`, `ATH_CDRK` names the `.xisf` master |
| D | 1811 (FITS twin) | FITS | — | the control |

So a light calibrated against the XISF master equals one calibrated against
the FITS master of the same build, bit for bit, and the XISF calibrated
output carries exactly the FITS output's samples.

## 4. Rebuild keeps its container (step 2, ruling R4)

With `calibration.master_format = fits`, `rebuild_master 1810` rewrote the
`.xisf` in place (mtime moved, signature still `XISF0100`, no `.fits`
sibling appeared beside the existing 1811 file); the catalog row still says
`XISF`.

## 5. The mixed-container stack (step 3b)

Two stacking runs with the global default config (LN off, drizzle off,
two-pass reference on), working/output folders under the work dir:

| Run | Set | Wall | Frames | Reference | Master light |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 37 | 205 pure (20 FITS) | 47 s | 20/20 calibrated, 20 measured | `…_0080` | `ACC_pure_20_NoFilter_mono_180s_20x.fits` |
| 38 | 204 mixed (10 FITS + 10 XISF) | 47 s | 20/20 calibrated, 20 measured | `…_0080` (the same source frame) | `ACC_mixed_20_NoFilter_mono_180s_20x.fits` |

**Master lights:** median 0.006155736 both, MAD 0.0001381743 both, mean
0.006621867 both, `max |diff| = 0`, 0.0000 % of pixels differ. The only
warning either run logged is `no plate solve on the reference frame; the
master has no WCS` (the copies carry no solve). No frame was excluded, no
warning mentions a container. The stacker takes FITS and XISF side by side
because stage 1 is the only stage that reads a source and funnels every
container into `c_*.fits`.

## 6. Incidents worth remembering

- **The system disk filled up.** The first export of the whole set (368
  frames) wrote 39 GB of float32 calibrated frames into the session
  scratchpad, which lives on the 228 GB system volume (it was at 99 %). The
  work dir now lives on `/Volumes/BigMac` and the harness README says so;
  the parity exports were redone on the 20-frame set (2 GB each). One `cp`
  that failed with `ENOSPC` left a truncated light in `pure/`; the export
  reported it honestly (`Failed to calibrate …: io: failed to fill whole
  buffer`), the file was re-copied and re-scanned in place (same frame id,
  the manual dark link survived) before the comparisons.
- **zsh's `path` variable is `$PATH`.** A loop that read `… | read fid fwhm
  path` emptied `PATH` for the rest of the script.
- `create_frame_set_from_selection`'s web route takes `frame_ids` in
  snake_case while its siblings take camelCase — a wire inconsistency, not
  fixed here.

## 7. Owed to the owner

- Open `acc-xisf/library/ATR2600M/MasterDark/master_dark_180s_-10C_g100_bin1x1_2025-04-12.xisf`
  in PixInsight; add it to WBPP as a master dark — it must be listed as a
  master (no "must be in XISF format" refusal), under DARK with exposure
  180 s. A light calibrated by WBPP with it should match Athenaeum's
  `export/C/…/c_*.xisf` of the same frame within noise.
- Open one `export/C/ACC pure 20/camera_atr2600m/lights/c_*.xisf` in
  PixInsight (a calibrated light; `bounds="0:1"`).
- The dark-flat case (`imageType="MasterDark"` on a `'Master Dark Flat'`
  keyword) was pinned by the unit test only — no raw dark-flat set with a
  linked flat was at hand in the copy; build one when a set exists and
  repeat the WBPP check.
