_The sync beta — the machine at your telescope sends each finished frame home by itself._

## What's New

- **Personal sync (beta).** Sign in on your main computer and on the machines at your telescopes with a single account — just your email and a one-time code — pair them together, and send frames directly between them over an encrypted peer-to-peer connection. There is no cloud storage: your data travels machine-to-machine through a lightweight relay and lands only where you send it. A new Transfers panel shows live progress, and the transfer history lists each package with device names, durations, transfer speeds, filenames, and a badge that tells you when a local copy is safe to delete. This is the first beta of the sync feature set — expect rough edges, and keep your originals until a transfer is confirmed.
- **Perseus — the capture-node agent (first release).** A small companion app for the computer sitting at the telescope. It watches your capture folders and automatically sends every finished frame to your primary Athenaeum machine, so your catalog fills in while you sleep. Perseus lives in the system tray with a live status icon and a one-click web dashboard where you can watch transfers, review history, tune how long it keeps local copies, and edit which folders it watches — and you sign in right from the browser. It can start automatically at login. Installers are provided for macOS (Apple Silicon and Intel, signed and notarized), Windows, and Debian/Ubuntu — including a headless build for Raspberry Pi (arm64) with an example systemd service.
- **Sync landing folders.** Incoming frames land in a dedicated folder you designate (or a sensible default), so received data stays apart from the rest of your library and is ready to be scanned into your catalog. A separate folder for collaboration data is available too.

## Changes

- Retention on a capture node is dry-run by default and only ever considers packages your primary machine has confirmed receiving. Nothing is deleted until you explicitly opt in, and never before it has safely arrived.
- The transfer history now shows durations, transfer speeds, original filenames, and friendly device names in place of raw identifiers.
- Perseus can watch several capture directories at once, and the list of watched folders is editable from its web page (applied on the next restart).
- Failed transfers can be retried directly from the Perseus web page.

## Bug Fixes

- Completed transfers no longer accumulate disk space on either side. Local payload copies are freed once the primary confirms receipt, and a sweep at startup reclaims anything left behind by an interrupted transfer.
- Frames are no longer lost or duplicated when a capture agent restarts in the middle of a transfer.
- Auto-sync now also picks up frames discovered by folder monitoring, not only manual scans.
- Signing in as your primary machine reliably starts listening for incoming frames.
- First-run sign-in no longer occasionally fails because of a device-key race.
- Retention refuses to remove a file unless it exactly matches what was transferred, and it audits every candidate before deleting anything.
- Perseus keeps syncing even if its web dashboard cannot claim its network port at startup.
- The Perseus transfer table no longer refreshes out from under a selection you are working with.
- Folder configuration now reports the actual conflict when a chosen folder overlaps another one.
