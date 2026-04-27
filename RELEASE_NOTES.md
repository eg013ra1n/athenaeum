## What's New

- **Plate solving** — astrometric plate solving wired into the desktop app via context + hooks; image frames can now be solved against the embedded star catalog and store WCS information for downstream features
- **Background folder monitoring** — long-running monitoring service that watches scan roots for new/changed files and updates the catalog without a manual rescan
- **Duplicates picker rule chain** — replaces the single master-root rule with an ordered, configurable chain. Default chain is `master_root` → `path_contains`; oldest-mtime and shortest-path rules are available from the panel's "Add rule" menu. Each rule abstains for groups it would empty, so the chain falls through safely instead of marking all copies for deletion
- **Path-contains rule** — new picking rule that marks any file whose path matches one of the user-defined substrings (e.g. `Backup`, `_copy`, `(1)`); supports a case-sensitive toggle and shows matched substrings underlined inside the file path
- **Per-rule live coverage** — the picking rules panel surfaces "N groups picked" next to each rule and "N / total groups picked" in the header so the user can see what each rule actually does in their data
- **Deep verify for duplicates** — opt-in byte-by-byte comparison before destructive operations, exposed as a Tauri command and an HTTP route. Mismatched files are auto-removed from the deletion plan; clean groups become safe to move

## Changes

- **Frame set clustering** switched from DBSCAN to a deterministic seed-and-grow single-link algorithm that recomputes the cluster center as the spherical mean after each addition — dithered/mosaicked fields now collapse into a single frame set even when the dither span exceeds the threshold
- **Grouping settings** keys renamed: `grouping.threshold.value` (default `3.0`) + `grouping.threshold.unit` (default `deg`, also accepts `arcmin` / `arcsec`)
- **Scanner**: paths that aren't valid UTF-8 are now rejected with a clear error instead of silently going through `Path::display()` and producing U+FFFD-corrupted strings that broke subsequent path-based DB lookups
- **Duplicates UI flatter layout** — group cards now share a single bordered container with row dividers (instead of each being its own card), and the "Picking rules" panel uses an accent-tinted background to clearly separate from the groups list
- **rustafits submodule** bumped to v1.0.0 with SVD-based affine fitting for plate-solve numerical stability

## Bug Fixes

- Fixed a focus race in the duplicates path-contains config where toggling the case-sensitive checkbox while the input was focused could silently drop a typed pattern (cross-element blur/click race overwriting state with stale closure values)
- Removed the noisy "Nothing marked" pill that rendered on every untouched duplicate group and clarified the deletion-status badges so the screen reads cleanly when many groups are open
