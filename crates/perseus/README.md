# Perseus — headless capture-node agent

Perseus is a small, headless companion to Athenaeum. It runs on the machine at
the telescope (a mini-PC, NUC, or Raspberry-class ARM board), watches one or
more capture directories, and streams every new sub-exposure to a paired primary
Athenaeum instance over an encrypted peer-to-peer link. No catalog and no desktop
UI — one binary, a TOML file, and an optional read-only [status page](#web-status-page),
meant to run as a `systemd` / `launchd` service.

It is deliberately lightweight: Perseus depends on `athenaeum-core` with
`default-features = false`, so it pulls **none** of the image-rendering
(`rustafits`) or plate-solving (`solvemyastro`) machinery. Only header parsing,
the package format, the sync engine, and the iroh transport come along.

## What it does

1. **Watch** one or more capture directories for new `.fits` / `.fit` / `.fts` /
   `.xisf` files.
2. **Wait** until each file has finished being written — capture software writes
   FITS progressively, so a file is only picked up once its size and mtime hold
   steady for `stability_secs` (default 10 s).
3. **Package** the frame: parse its header into portable metadata, hash it, and
   wrap it in a self-describing bundle (one package per file).
4. **Send** it to the paired primary and record the peer's per-frame receipt.
   Delivery is durable — an unfinished transfer resumes after a crash or restart
   (state lives in `<data_dir>/perseus.db`).

Retention (deleting local frames after the peer confirms them) is active, but
**dry-run by default** and gated behind an explicit soak opt-in. In dry-run every
would-delete is logged and nothing is removed. Real deletion begins only when the
owner sets BOTH `dry_run = false` AND `i_have_verified_the_soak = true` after the
M-Perseus-MVP soak sign-off. Only *confirmed* (fully received by the primary)
source frames are ever eligible — under any policy, at any disk pressure.

## Install

Build the release binary (on the capture machine, or cross-compiled):

```bash
cargo build -p perseus --release
# binary at target/release/perseus
sudo install -m 0755 target/release/perseus /usr/local/bin/perseus
```

ARM (Raspberry Pi 4 / CM4, aarch64 Linux):

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build -p perseus --release --target aarch64-unknown-linux-gnu
```

## Configure

Create `perseus.toml` (see `--config` below for the path). This is the binding
contract:

```toml
# perseus.toml
capture_dir = "/data/capture"            # directory your capture app writes to
# capture_dirs = ["/data/cam1", "/data/cam2"]  # …or watch several (see note below)
data_dir = "/var/lib/perseus"            # SQLite store + blob store + device key + logs
pairing_ticket = "<paste from primary>"  # primary → Settings → Sync (dev)
mode = "auto"                            # only value in the MVP

# Local status page (optional; top-level keys — must precede any table):
# web_bind  = "127.0.0.1:8686"           # bind address (default); "" disables the page
# web_token = "<random secret>"          # REQUIRED when web_bind is NOT loopback

[retention]
policy = "keep_everything"               # keep_everything | on_confirm | keep_days | disk_pct
dry_run = true                           # safe default; see the go-live note below
# i_have_verified_the_soak = true        # REQUIRED to allow dry_run = false

# Optional tuning (defaults shown; omit to accept them):
# stability_secs = 10                    # write-quiet window before a file is sent
# poll_interval_secs = 2                 # how often pending files are re-checked
# keep_days = 30                         # only for policy = "keep_days"
# disk_max_pct = 90                      # only for policy = "disk_pct"
# interval_secs = 3600                   # retention evaluation cadence (hourly)
```

Notes:

- **One or several capture directories.** Use `capture_dir = "…"` for a single
  directory, or `capture_dirs = ["…", "…"]` to watch several at once (e.g. two
  cameras writing to separate folders). Set **exactly one** of the two forms —
  configuring both, or neither, is rejected at startup. Perseus arms one watcher
  per directory; every new frame from any of them flows through the same
  packaging, send, and retention pipeline.
- **`pairing_ticket`** is the out-of-band pairing string from the primary. On
  first `run`, Perseus generates a device key at `<data_dir>/device_key` (mode
  `0600`) and derives its own node identity from it. Both keys persist, so the
  pairing survives restarts.
- **Going live is a two-key edit.** `dry_run = false` is rejected on its own —
  the config must ALSO set `i_have_verified_the_soak = true`. This makes enabling
  irreversible deletion a conscious, greppable acknowledgement that the
  M-Perseus-MVP soak has been signed off. Until you add both, retention runs in
  dry-run (logs would-deletes, removes nothing).
- **Only confirmed frames are ever deleted.** No policy and no disk-pressure
  setting can ever delete a frame the primary has not fully received.
- The `[retention]` table may be omitted entirely — it defaults to
  `keep_everything` / `dry_run = true` / `i_have_verified_the_soak = false`.

## Run

```bash
# Foreground service mode (watches + sends until Ctrl-C):
perseus --config /etc/perseus/perseus.toml run

# One-shot status (config summary + in-flight transfers), prints to stdout:
perseus --config /etc/perseus/perseus.toml status

# Enqueue files that already existed before the watcher was running, then drain:
perseus --config /etc/perseus/perseus.toml enqueue-backlog /data/capture/2026-07-06
```

`--config` defaults to `perseus.toml` in the working directory.

### Subcommands

| Command                    | What it does                                                                                             |
| -------------------------- | ------------------------------------------------------------------------------------------------------- |
| `run`                      | Arms the watcher and the sync engine; sends every new frame; resumes unfinished transfers on start.     |
| `status`                   | Reads the store and prints config + a table of in-flight (non-terminal) packages. Starts nothing.       |
| `enqueue-backlog <dir>`    | Enqueues every eligible file already under `<dir>`, then waits until they drain (or Ctrl-C) and exits.   |

The watcher (in `run`) only sends files that **appear after it starts** — files
already present when it launches are treated as a baseline and skipped, so a
restart never re-sends your whole capture directory. Use `enqueue-backlog` to
send pre-existing files on purpose.

## Web status page

`run` serves a small, read-only status page for eyeballing a headless node
without SSH. It is **on by default on loopback** — open `http://127.0.0.1:8686/`
on the capture machine.

- **`web_bind`** (default `127.0.0.1:8686`) — the bind address. Set it to `""`
  to disable the page entirely.
- **`web_token`** — a bearer token. It is **required** for any non-loopback bind
  (e.g. `0.0.0.0:8686` or a LAN address): Perseus **refuses to start** if you
  expose the page off loopback without one, so the page is never silently
  wide-open. On loopback a token is optional. When a token is set, the page asks
  for it on first load (a `401` triggers a browser prompt) and remembers it in
  the browser's `localStorage`.
- A **runtime bind conflict is non-fatal** — if the port is already in use, the
  agent logs a warning and keeps running (watch/send/retention are unaffected);
  only the page is unavailable.

The page has four sections:

| Section       | Shows                                                                                             |
| ------------- | ------------------------------------------------------------------------------------------------- |
| **Status**    | The watched capture directories, live in-flight transfers, current retention policy, and counts.  |
| **Sent**      | Outbound packages, newest first, with per-row **Delete** on confirmed packages only.              |
| **History**   | The transfer audit log — filename, OBJECT, peer **device name** (from the hub, when known), size, duration, outcome, and a **✓ safe to delete** marker on peer-accepted frames. Filterable by filename. |
| **Retention** | The live retention policy (editable) plus a rolling log of recent retention passes.               |

**Delete semantics.** Manual delete removes the *source capture file* of a
package, and only ever for a **confirmed** package (one the primary has fully
received) — the button is absent on any other row. It goes through the exact same
confirmed-only, audit-before-delete path that retention uses; the web page cannot
delete anything retention couldn't.

**Retention edits are safe by construction.** You can change `policy`,
`keep_days`, `disk_max_pct`, `interval_secs`, and `dry_run` from the page. But the
**two live-deletion keys stay TOML-only by design**: `i_have_verified_the_soak` is
never web-writable, and flipping `dry_run = false` from the page is rejected
(422) unless the on-disk soak opt-in is already `true`. Going live therefore
remains the deliberate two-key hand edit described above — the UI can never enable
irreversible deletion.

Full design: [`docs/superpowers/specs/2026-07-08-stage15-sync-hardening-design.md`](../../docs/superpowers/specs/2026-07-08-stage15-sync-hardening-design.md).

## Service setup

Sample unit files live in [`dist/`](dist/). Adjust the binary/config paths, the
run-as user, and the writable directories to your layout.

### Linux (systemd)

```bash
sudo install -m 0644 dist/perseus.service /etc/systemd/system/perseus.service
sudo mkdir -p /etc/perseus && sudo install -m 0640 perseus.toml /etc/perseus/perseus.toml
sudo useradd --system --home /var/lib/perseus --shell /usr/sbin/nologin perseus || true
sudo mkdir -p /var/lib/perseus && sudo chown perseus:perseus /var/lib/perseus
sudo systemctl daemon-reload
sudo systemctl enable --now perseus.service
journalctl -u perseus -f
```

`Restart=on-failure` (with a 10 s back-off) plus crash-resume means a power blip
or a killed process picks up exactly where it left off.

### macOS (launchd)

```bash
sudo install -m 0644 dist/com.athenaeum.perseus.plist \
  /Library/LaunchDaemons/com.athenaeum.perseus.plist
sudo mkdir -p /usr/local/etc/perseus && sudo cp perseus.toml /usr/local/etc/perseus/
sudo launchctl load /Library/LaunchDaemons/com.athenaeum.perseus.plist
```

`KeepAlive` restarts Perseus if it exits non-clean; `ThrottleInterval` mirrors
the systemd back-off.

## Logging

Perseus writes structured JSONL logs to `<data_dir>/logs/perseus.<date>.jsonl`
(daily rotation, 14 files retained), plus a human line to stderr (captured by
journald / launchd log files). The default filter keeps Perseus's own modules at
`info` but quiets iroh's internals (`iroh`, `iroh_relay`, `iroh_blobs`,
`net_report`, `portmapper`, `netwatch`, `noq_udp`) to `warn` — otherwise their
transport/probe span-close events are >99% of the log volume. Raise the level
without editing the config via `ATHENAEUM_LOG` (full [`EnvFilter`] syntax, which
overrides the default entirely — including the iroh quieting):

```bash
ATHENAEUM_LOG=info,perseus=debug perseus --config perseus.toml run   # more of our own detail
ATHENAEUM_LOG=info,iroh=debug  perseus --config perseus.toml run     # un-quiet iroh internals
```

Only `status` output and `--help` go to stdout; everything operational is a
tracing event — there is no ad-hoc `println!` in the service path.

[`EnvFilter`]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html

## Data directory layout

```text
<data_dir>/
  device_key          32-byte ed25519 secret (mode 0600) — this node's identity
  perseus.db          durable sync store (outbound state machine + history)
  sync_blobs/         iroh content-addressed blob store
  packages/           staged package directories (one per frame)
  logs/               rolling JSONL logs
```

`sync_blobs/` does not grow without bound: each package's blob data is released
once the transfer is confirmed/acked, and a startup sweep retires any tags left
stale by an earlier crash — so steady-state disk use tracks in-flight frames, not
lifetime volume.
