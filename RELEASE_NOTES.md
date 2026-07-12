_The mesh beta — send frames between any of your machines, and only the new ones cross the wire._

## What's New

- **Send anywhere — the sync mesh.** The one-primary model is gone: every machine signed in to your account is now a full peer. You choose exactly where each transfer goes — send from the observatory to the studio, from the studio to a backup machine, or to several destinations at once. Devices carry a capability instead of a role: an Athenaeum install can send and receive, a Perseus capture node is send-only.
- **Send to… from the app.** Select frames in the frame table — or files and folders in the file browser — and send them straight to another of your machines. Pick one or more destinations in the new dialog; the Transfers panel tracks progress, and the finished notification tells you how many frames were new and how many the destination already had.
- **Duplicate-aware transfers.** Before any data moves, the two machines compare notes: frames already in the destination's catalog are skipped, and only new frames cross the wire. A frame is only ever skipped after its full content hash matches — never on a guess. Re-sending an overlapping batch transfers just the difference.
- **Perseus sends in batches.** Instead of firing every file the moment it lands, Perseus now accumulates finished frames and sends them as a single package — automatically once the camera has been quiet for a while, or manually with a Send-now button. The web page gained a "To sync" tree showing exactly what is waiting, an auto/manual toggle, and a history grouped by batch.
- **Tidy landing.** Received frames land in a folder named after the sending machine, mirroring the sender's own folder layout — data arriving from several machines never mixes.

## Changes

- Every device has a unique name in your account (new machines default to their hostname). Rename it inline from the app's Account section or the Perseus web page; the name keys the destination picker and the landing folder.
- The sync channel and the Perseus web dashboard were hardened after an internal security review: stricter validation of incoming package identifiers, per-peer authorization on the receiving side, a stronger web-session model, and tighter permissions on the device key file.
- The sidebar sync indicator now appears only while the transport is actually running.

## Bug Fixes

- A package sent to several destinations is no longer cleaned up after the first confirmation — a destination that was offline can still finish its transfer later, with no silent data loss.
- Retrying a failed transfer from the Perseus web page can no longer free a payload that other destinations still need.
- Perseus marks a capture file as handled only once it has actually been packaged — a file that misses one batch is picked up by the next, never silently skipped.
- Sending a folder no longer sweeps in sibling folders whose names share the same prefix.
- The active-transfer count is no longer inflated when sending to several destinations at once.
- The Perseus web session token is kept only for the lifetime of the browser tab.
