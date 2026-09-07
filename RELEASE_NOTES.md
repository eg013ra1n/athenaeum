*Master builds finish in a fraction of the time, Full Resolution finally means it for colour cameras, and every page remembers where you were.*

## What's New

- **Master builds are about six times faster.** A 100-frame bias set that
  took 233 seconds now takes 40, measured cold on a spinning disk with the
  page cache cleared. The integration engine sizes its working band from the
  machine's actual memory instead of a fixed constant, keeps that band in
  the frames' own pixel format rather than widening everything to float on
  the way in, and reads the frames of a band in parallel by position. Nothing
  about the result changes: the same masters, pixel for pixel.
- **A build now tells you what it is doing.** The calibration tree shows the
  running build's stage and percentage, and each build writes a line to the
  log when it starts and when it finishes, with its duration, read and
  combine times, and throughput. A thirteen-minute build used to be
  indistinguishable from a hung one.
- **Full Resolution means native resolution for colour frames.** In Blink,
  every one-shot-colour frame was rendered at half size even with the
  full-resolution button pressed, because the fast debayer folds each 2×2
  sensor tile into a single pixel. A 6248×4176 frame arrived as 3124×2088,
  which made 1:1 star inspection — the reason that button exists —
  impossible. Full Resolution now debayers at the native grid.
- **Back and forward, everywhere.** Every page header carries a back/forward
  pair, and Backspace, Alt+← / Alt+→ and, on macOS, ⌘← / ⌘→ step through
  them. Returning to a page returns you to it: the File Manager, Objects and
  Project Detail tabs come back as you left them, and the scroll position is
  restored per history entry. History is per session, so a restart starts
  fresh.
- **The plate-solve input gate is in Settings.** The v0.5.5 notes said the
  thresholds were there; they were not, and the sentence was retracted. They
  are now: an **Input Gate** section with its toggle and both thresholds,
  plus **Batch Concurrency** in Solver Parameters. The copy states the part
  the numbers are useless without — that both conditions must hold at once,
  and what the values are on frames that do and do not solve.

## Changes

- **Folders:** an offline folder's banner has a **Check again** button, so a
  drive you have just remounted is re-detected on the spot instead of
  needing you to scan some other folder and let it re-check everything as a
  side effect. A file that failed to parse during a scan gets a reveal
  button next to its error, which opens the file manager at that file.
- **Settings → Calibration → Master Build Memory:** a memory budget for the
  integration engine. It explains rather than merely displays: if what you
  typed is not what is applied — clamped to the supported range, divided
  across concurrent jobs, or both — it says so and shows the figure actually
  in force.
- **Settings → Blink Viewer:** a memory limit for the preview cache, which
  matters now that a full-resolution colour render is four times the size it
  used to be.
- **Windows is measured, not assumed.** The whole test suite now runs on
  Windows in continuous integration and blocks a broken build. Getting there
  fixed one real defect that shipped: a received file's path was stored with
  a mixed path separator, which is corrected below.

## Bug Fixes

- **Deleting a missing master's file no longer strands its raw frames.**
  Removing a master's file from the Missing Files panel deleted the row
  directly, which left the master's empty shell behind and left the raw
  calibration set still marked as superseded by it. Those raw frames became
  invisible to calibration matching permanently, superseded by a master that
  existed neither on disk nor in the catalog, with nothing in the interface
  left to undo it. That route now goes through the same un-supersede path
  every other way of deleting a master already used.
- **Windows: a received file's path was stored with a mixed separator.**
  Transfers landed catalog rows whose paths mixed both separators, so later
  lookups against the same file could miss. Project contributions had the
  same shape and are fixed with it.
- **Windows: a relative path carrying a drive letter is refused.** The guard
  only checked the front of the path, so a drive-letter segment further in
  was accepted.
- A full-resolution colour render in the web build now holds its slot for
  the whole render rather than releasing it early, and the preview cache
  limit is clamped on the backend, so a value typed into Settings cannot put
  the render path into a state the interface would not allow.
