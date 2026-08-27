*Duplicate detection that actually finds your duplicates — and never invents them.*

## What's New

- **Duplicate detection has been rebuilt around what a frame is, not where it
  sits.** The Duplicates view used to group files by size, timestamp and
  filename — and a copy's timestamp changes the moment it crosses a drive or
  an exFAT stick, so on real libraries the view quietly found nothing. Raw
  sub-frames are now matched by their stored FITS/XISF header, which every
  scan already records: copies match no matter how many times they were
  moved, and on a 42 000-file test catalog the view went from 0 groups to
  ~2 750 (170 GB of reclaimable space).
- **Masters and processed files are matched by their full contents.**
  Processing tools copy a header verbatim onto a different image, so a header
  can never identify a master. Instead, the scan hashes the handful of
  master candidates whose headers collide (a minute of work, not a
  library-wide pass), and only byte-identical files are ever grouped. Two
  different stacks that share a header will never be offered as copies of
  each other.
- **Deep verify remembers its work.** Verifying a duplicate pair already
  reads both files byte by byte; that read is no longer thrown away. Files
  proven identical keep their full-content hash in the catalog, so
  re-verifying them is instant, and the stored hashes feed the master
  matching above for free.

## Changes

- Folder similarity uses the same header identity as the Duplicates view, so
  the two screens agree about the same pair of folders.
- The Settings text for duplicate detection now describes what each mode
  actually does, including an honest caveat about sampled hashing for
  masters in content mode.
- Duplicate grouping is protective by design: a renamed true copy, a file
  with an unreadable header, or an unclassified frame type is skipped rather
  than guessed at. A miss costs a re-scan; a wrong group could cost a frame.

## Bug Fixes

- Calibration sets and sessions no longer keep counting frames that were
  deleted: removing duplicates now updates the set's frame count everywhere,
  and existing catalogs are corrected once on the next start.
- Changing the keep rules while a deep verify was running used to hide the
  progress bar while the verification kept reading disks in the background —
  and then popped up a summary for a run you had discarded. It now stops the
  run properly, and cancel works even if you press it just before a rules
  change.
- Processed files that keep their source's header (e.g. background-extracted
  copies saved next to the original) are no longer grouped with it: the
  grouping key includes the filename, and their divergent names keep them
  apart.
