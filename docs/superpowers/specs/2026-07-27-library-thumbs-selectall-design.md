# Perseus Library: inline thumbnails + select-all (2026-07-27)

Two visual upgrades to the Library tab (`crates/perseus/src/web/{app.js,style.css}`
only — no Rust, no new endpoints).

## §1 Thumbnails left of the file name

Each file row gains a ~72px thumbnail cell before the name (dirs get an empty
cell). Loading is lazy and automatic: an IntersectionObserver enqueues rows as
they scroll into view (200px margin) and a client-side queue fetches them ONE
at a time — the server's preview renderer is `Semaphore(1)` anyway, so a
serial client queue keeps the pipe drained without stacking hundreds of
pending requests. Fetches reuse the pre-blink discipline: bearer `api()` →
`blob:` object URL (an `<img src>` cannot authenticate), guarded by a
generation counter (`libThumbSeq`) so a directory/root change drops in-flight
results, clears the queue, and revokes every object URL (no leaks; re-visits
are cheap — the server preview cache keys on width and answers 304 before the
render gate, and the browser HTTP cache holds the bytes). Thumbs render at one
fixed width (`LIB_THUMB_WIDTH = 96`, above the server's MIN 64) so each frame
occupies exactly one server cache entry shape. A frame the renderer refuses
(415/404/…) shows a static no-preview glyph and is negative-cached for the
tab's lifetime — no retry storms. Clicking a thumbnail opens the existing
pre-blink pane (same `data-lib-pv` delegation as the file name).

Named trade-off (accepted): first browse of a big folder on weak hardware is N
background renders trickling in serially, visible rows only. That is what
"glance and delete" requires; hover-only was rejected.

## §2 Select-all

A tri-state master checkbox in the selection column header: click selects /
deselects every pickable row of the CURRENT listing (files and dirs — exactly
what manual clicking can reach), driving the existing `libTogglePick` per row
so the cross-directory selection model and the footer stay authoritative.
States: checked = all listed rows picked, indeterminate = some, empty = none;
recomputed after every per-row toggle and every listing render. Per-row
checkboxes and their no-refocus semantics are untouched.
