# Perseus Library: blink mode in the preview pane (2026-07-27)

The pre-blink pane grows into a real blink viewer, Athenaeum-style (`app.js` +
`style.css` only; no Rust).

- **Playback**: ▶/⏸ (Space) + speed select (0.5/1/2/4 fps) in the pane head.
  Autoplay advances ONLY when the next frame is already decoded (a spinner
  mid-run breaks the comparison blink exists for); a cache miss polls every
  150 ms until the render lands. First pass through a folder is render-bound
  (server `Semaphore(1)`); subsequent passes play from cache. Stop at the end
  (existing stop-at-ends stance). Manual ←/→ keeps working and re-arms the
  timer.
- **Prefetch**: while the pane is open, a serial queue (same discipline as the
  thumbnails) fetches +3 ahead / −1 behind at full preview width into a
  10-entry blob LRU (`libPvCache`); evicted and closed entries revoke their
  object URLs, the displayed frame is never evicted. Renderer refusals share
  the thumbnails' negative cache. A generation counter (`libPvGen`, bumped on
  open/close) orphans in-flight prefetches; the per-show `libPvSeq` machinery
  is untouched.
- **Marking = the existing selection.** `X` (or the Mark button) toggles the
  current frame in `libState.selected` via `libTogglePick` — the same set the
  listing checkboxes and footer drive; the listing row's checkbox and the
  select-all tri-state sync in place. A marked frame shows a red outline +
  "marked" chip.
- **Delete N** in the head opens the EXISTING two-step delete dialog over the
  whole selection. Per the repo's single-overlay invariant, `libDlgOpen`
  closes the pane first — matching the mark-through-the-night-then-delete
  flow. Deleted frames left in a later pane walk are caught by the existing
  file-gone (404 → one listing re-read) handling.
- Keys: ←/→ step (existing), Space play/pause, X mark, Esc close (existing).
  `e.code` (layout-independent) for Space/X; modifiers still yield to the
  browser.
