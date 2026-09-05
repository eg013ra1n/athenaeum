*Calibration is a stage of export now, choosing it by hand is one screen, a night that runs past midnight is one night again — and plate solving no longer invents a position for a frame full of streaks.*

## What's New

- **Calibrated lights are made at export time.** The **Calibrated lights**
  export mode now calibrates every light on the spot from your built masters
  — at export and at send — instead of requiring a separate *Calibrate
  Lights* run first. The standalone flow, its dialog and its per-frame badge
  are gone. Two new toggles ride along, both on by default: **hot-pixel
  correction**, which replaces known-defective pixels using the master dark,
  and a **full-resolution VNG debayer** for one-shot-color cameras.
- **Choosing calibration by hand is one screen.** Light frames and a
  calibration set's own sub-calibration used to open two different dialogs;
  they are now the same picker. The camera, exposure and date filters are
  always visible, clicking a value in the left panel filters the list by it,
  and each card names the difference that matters — *"Offset 30 → 200"* —
  instead of listing parameters you had to compare yourself. Cards carry the
  shooting window, not just the date, so two sets shot the same evening are
  finally distinguishable.
- **A frame with no coordinates can be solved by naming its target.** Set
  OBJECT in the metadata editor and the editor tells you, as you type,
  whether the name is one the sky catalog knows; plate solving then starts
  from that position instead of searching blind.
- **Sending returns instantly.** A transfer appears right away as a
  *preparing* row with a live byte count, speed and a Cancel button while
  the files are staged in the background, instead of the dialog hanging
  until every file has been copied.
- **A transfer costs one copy of its files on each machine instead of two.**
  The sender serves the prepared package where it lies and the receiver
  links the downloaded files into place, so a 20 GB send no longer needs
  40 GB of free space at either end.
- **Settings has a Transfers tab.** The outgoing staging folder and the
  incoming working folder can each be pointed at any disk — with the upload
  limit, receiving limit and storage figures moved there too — and whatever
  the previous folders still hold can be cleaned up from the same page.

## Changes

- **Analysis tab:** every column with data now sorts, WCS and Reference
  included. The WCS column reads *Header* / *ATH* / *—* instead of icon
  badges, and the Reference column is just the star: filled on the chosen
  frame, a star button on the rest.
- **Export tab:** the folder tree, file total and size estimate follow the
  selected mode — no calibration folders in *Lights only*, `c_*` names for
  *Calibrated lights*, one file per master set — and a remembered mode that
  is not available for the current set no longer stays selected. A set with
  no calibration linked at all now offers only *Lights only*, since the two
  raw modes would have landed the same files under a different name. *Lights
  only* shows no calibration warnings, and a missing-calibration warning
  names the camera when two groups share a filter.
- **Objects:** a new **Recalculate nights** button repairs sets whose nights
  were stitched together by a merge before this release.
- **Calendar:** a day's cards show the camera, the telescope and the
  first–last exposure time of that night.
- **Transfers:** the sender's progress line counts only the files that
  actually travel and says how many the receiver already had — *"84 of 300
  files · 262 already on peer"* instead of *"346 of 562"* — so both sides
  show the same total and the bar no longer stalls at half when most of a
  set is already there.
- **Master libraries:** a master with no GAIN or OFFSET in its header is
  flagged in the Dark/Flat library. Such a set can never be matched
  automatically, and *Edit Metadata* fills the values in.
- **Collaboration:** publishing your own lights to a project is temporarily
  disabled while the calibrated-export changes above are worked into it.
  Receiving other members' contributions is unaffected.
- Known consequence: resending a *Calibrated lights* transfer after
  re-calibrating (for example after rebuilding a master) now lands a second
  copy on the receiver instead of replacing the first one — there is no
  tracking table left to deduplicate against.

## Bug Fixes

- **Plate solving no longer invents astrometry for frames whose stars are
  streaks.** Wind-shaken frames used to come back *solved* at 16–193× their
  true pixel scale — a confident, entirely wrong position written into the
  catalog. Such detections are now excluded from matching, and any solve
  whose scale disagrees with the frame's own focal length and pixel size is
  refused.
- A frame whose analysis shows badly trailed stars is skipped with a plain
  reason instead of spending minutes failing, and the thresholds are in
  Settings if you want them looser. A frame that turns out to be nothing but
  streaks is refused within a second — naming how many of its detections
  were streaks — rather than searching for minutes.
- **A night that runs past midnight is one night again.** The night tree
  grouped by the frame's calendar date, so every session through midnight
  showed as two — *"October 18"* and *"October 19"* instead of *"October
  18–19, 2025"* — and the Shoot Calendar split the same night across two day
  cells. Both now group by the imaging night, which lands on the day it
  started.
- Merging frame sets (and *Find new images*) now recomputes the merged set's
  nights from all of its frames instead of stitching the two sets' night
  rows together, so a night split by a meridian-flip re-pointing no longer
  shows up as two.
- **The manual calibration list is usable again.** Every candidate carries a
  real closeness percentage instead of a flat 0 % for anything your matching
  rules refuse, the list is ordered by how near a miss each one is — same
  camera first, then by what each broken rule costs the calibration — and
  every card states why a set was refused (*"Temperature: 19.4 vs −9.9 — off
  by 29.3, limit 5.0"*, *"Gain: this set does not declare one"*) instead of
  showing an unexplained score.
- *Show only compatible* now means exactly that. It used to hide everything
  whenever no candidate was perfect, and it hid compatible-but-old sets as
  well.
- The export size estimate now uses the set's real average file size instead
  of a fixed 50 MB per file.
