_The delivery beta — transfers you can watch, steer, and trust to finish._

## What's New

- **The Transfers screen.** A new torrent-style view of everything moving between your machines: every batch with live per-file progress bars, transferred bytes, current speed and state — incoming and outgoing alike. Expand a batch to see each file; a History tab keeps finished, cancelled and failed batches grouped per batch with an honest reason on every row that stalled or failed.
- **Delivery that doesn't give up.** A transfer to a machine that is offline, asleep or unreachable no longer fails — it waits and retries with a growing back-off, forever, and the row shows a countdown to the next attempt. The moment the destination comes back (or the relay reconnects), pending packages are kicked immediately instead of waiting out the timer. A "Send now" button skips the countdown on demand.
- **Cancel from either side.** Both the sender and the receiver can cancel a transfer. Cancelling on the receiving end tells the sender, which stops retrying and marks the batch cancelled — and a cancelled batch keeps its payload, so it can simply be retried later.
- **Perseus transfer controls.** The capture agent's web page gained the same powers: per-batch progress with retry countdowns, a stalled badge, Send-now/kick, cancel, retryable cancelled rows, and a device picker for choosing send targets.
- **Sturdier transport.** The app and Perseus now each run a single sync transport for the whole process, which removes the device-identity conflicts that could kick a machine off the relay when several parts of the app talked at once. Machines also publish their current addresses to your account, so peers dial each other directly and reconnect faster after network changes.
- **Collaboration preview.** This beta carries the first look at shared projects: joining a project, linking a frame set to it, publishing calibrated frames for the coordinator's review, receiving approved contributions from teammates, and exporting the whole project for stacking. The server side is still rolling out — the Projects page will come alive for accounts over the coming days, no update needed.

## Changes

- Received files that would land inside the app's own data folder now raise a standing warning with a one-click way to pick a proper landing folder.
- Incoming folders are named after the sending machine, with a safe fallback when a device name has no usable characters.
- Transfer notifications and rows deduplicate cleanly when a batch reaches its final state.

## Bug Fixes

- **Master calibration builds no longer fail** with `non-printable-ASCII string value for ATH_REJ`. A single non-ASCII character in an internally generated header value aborted every master build at the final write — after all the integration work was already done. Header writing is now also safe for file paths and names in any language (non-ASCII characters degrade to `?` placeholders in the FITS header instead of failing the build).
- Deleting a scan root no longer errors when master calibration provenance references frames under it.
- Renamed send targets in Perseus self-heal to device ids instead of failing the batch.
- Transfers get a stable package identity on the wire, and stale in-progress incoming rows are reconciled on startup instead of lingering forever.
- Upload progress for multi-file batches accumulates correctly instead of resetting between files.
