*Send an object to another node straight from its Export tab — lights only, with calibration frames, with masters, or already calibrated — and scans that never hash a file again.*

## What's New

- **Send an object to another node from the Export tab.** The tab now offers
  four compositions, each with a file count and its own readiness check:
  **Lights only** (new), **Lights + calibration sets**, **Lights + masters**
  and **Calibrated lights**. A new **Send to node…** button sends the set as
  one package in the same WBPP folder layout the export produces
  (`camera_<x>/BIAS_…/DARKS_…/FLAT_…/lights`), so the batch folder opens in
  WBPP on the receiving device as-is. Nothing changes about the export itself:
  the four modes and **Export to WBPP** share the same readiness rule.
- **The receiver integrates what it gets.** Raw calibration frames and master
  files that arrive over a transfer now become calibration sets on the
  receiving device automatically — until now they landed as bare frames that
  no light could be matched against. Calibrated lights (`c_*.fits`) land
  outside the catalog as calibrated artifacts, exactly as they live on the
  sender: when the source light is cataloged on the receiver too, it shows
  as *calibrated* there. Re-sending a batch is deduplicated — no `_2` copies.
- **One readiness gate for export and send.** *Lights + masters* is strict:
  it is refused while any linked calibration set still lacks a master, and
  the message links to the Coverage tab with that set highlighted.
  *Calibrated lights* requires every light to have a fresh calibrated output.
  The tab never builds masters or calibrates lights for you — only ready
  material is exportable or sendable; the preparation happens on the
  Coverage tab, one click away.
- **Scans read headers only.** The content index that powers transfer
  deduplication and content-based duplicate grouping is built by its own
  background job — after every scan when sync is set up or content grouping
  is on, and on demand from a new **Build content index** button on the
  Folders page. Settings gained one *Content index* card in place of two.
- **Transfers remember the files they hash.** Every full read a send, a
  receive or a deep verify pays is kept in the catalog, so masters that
  travelled between devices show up in duplicate detection without another
  read, and re-sending files a device has already confirmed costs no disk
  I/O on the receiver.

## Changes

- **The app never deletes a sent source.** The desktop and web app carried a
  hidden retention loop that could reclaim files after a confirmed send; it
  is gone. Retention is a Perseus-only concept — the capture agent's per-file
  fate rules are unchanged.
- **Lights Analysis tab.** With nothing selected the Blink button now reads
  **Blink all** and blinks every displayed frame; with a selection it reads
  **Blink** and blinks that. The frame table supports Shift-click range
  selection, and a plain click anywhere on a row toggles it, the way the file
  browser already works. The tab's *Send to…* button is gone — on an object
  page, **Send to node…** on the Export tab is the one place to send from;
  the file browser's selection send is unchanged.
- The catalog drops a duplicate-detection column it no longer uses; existing
  catalogs are migrated once on startup.

## Bug Fixes

- A master recognised only by its filename (a `master_dark_*.fits` whose
  `IMAGETYP` still reads *Dark*) travelled as a raw frame when sent from the
  frame table. It now travels as a master.
- A calibrated light the receiver refuses is removed from disk with the
  reason recorded, instead of lingering as an untracked file that a later
  scan would re-adopt — and an unreadable payload is reported as a read
  failure, not as "not a calibrated light".
- The desktop app now logs a refused export the way the web build already
  did; a readiness or catalog failure could abort an export with nothing in
  the log.
