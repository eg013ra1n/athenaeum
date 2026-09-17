*Athenaeum v0.6.4: the application updates itself — a new version is offered inside the app with its release notes, downloaded on your click, verified, installed and restarted, on macOS, Windows and Linux.*

This is the first release that can be received without visiting the download
page. It is also, by necessity, the last one you install by hand: a copy of
v0.6.3 or older knows nothing about the new mechanism, so this one comes from
the download page as before, and every release after it arrives through the
app. Underneath it is the release machinery that v0.6.4-beta.1 rebuilt and
exercised — every download is verified before it is announced, and now every
update is signed before it is offered.

## What's New

- **Updates inside the application.** At launch Athenaeum checks for a newer
  version (this is the existing *Automatically check for updates on startup* setting,
  on by default). If there is one you get a toast, a bell entry and an
  **Update available** dialog with the full release notes. Nothing is
  downloaded until you click **Download and install**; a progress bar shows
  the bytes, and when the package is in place you choose **Restart now** or
  **Later**. On macOS the application bundle is replaced, on Linux the
  AppImage, on Windows the installer runs by itself and relaunches the app.
  Every package is checked against a signature the build produced; a package
  that fails that check is refused before anything is touched, and the
  download page stays one click away as the fallback.
- **What's new, once.** The first launch of a new version opens a **What's
  new** dialog with that version's own notes. It appears once per version;
  **About → View release notes** shows the same text again at any time, and
  works offline.
- **Docker installs are told too.** The web UI runs the same check and shows
  the same notes, with the `docker pull vsharifov/athenaeum:<version>` line
  and a copy button in place of an install button — the image itself is
  still pulled by whoever runs the container.
- **A soft guard before restarting.** If a stacking run, a master build or a
  transfer is in progress, the dialog says so before you restart. It never
  blocks you; it just makes sure nothing is cancelled by surprise.

## Changes

- **Signed updates.** Every updater package is signed at build time and its
  signature is published beside it. The application carries the matching
  public key and refuses anything that does not verify. The release pipeline
  verifies the same signatures itself, from the public addresses, before the
  update feed moves — a release that fails that check never becomes the one
  the app offers.
- **Two update channels.** The stable channel is the default; *Check for beta
  updates* in Settings switches the check to the beta feed. A beta is never
  offered to a stable install.
- **Linux package installs stay manual.** An install from the `.deb` (or a
  distribution package) is not updated in place — the app opens the
  download page instead. The AppImage updates itself. Package-manager
  updates are a scoped follow-up.
- **The release machinery from v0.6.4-beta.1**, now behind a stable release
  for the first time: every installer, both Docker architectures and the
  release post are fetched back from their public addresses, and the macOS
  disk images pass the same Gatekeeper check a new Mac performs, before
  anything is announced or the version-less *latest* links move. Every
  release keeps a permanent download folder of its own. One naming scheme
  for every download across all three operating systems
  (`athenaeum-0.6.4-macos-arm64.dmg`: product, version, system, processor
  architecture — `x64` and `arm64` everywhere); the previous link spellings
  keep working until v0.7.0. The download page and this post are generated
  from the release notes, so the site cannot describe a release differently
  from the release.

## Bug Fixes

- No application code changed between v0.6.3 and v0.6.4 apart from the
  update mechanism above. Two things around it were fixed along the way:
  the macOS disk images were being notarized twice per release, and the
  slowest database tests on Windows were running with a stricter disk-sync
  setting than the application itself uses, turning a one-second test into
  a minute.
