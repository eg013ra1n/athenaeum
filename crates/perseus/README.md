# Perseus — headless capture-node agent

Perseus is a small, headless companion to Athenaeum. It runs on the machine at
the telescope (a mini-PC, NUC, or Raspberry-class ARM board), watches your
capture directory, and streams every new sub-exposure to a paired primary
Athenaeum instance over an encrypted peer-to-peer link. No UI, no catalog — one
binary and a TOML file, meant to run as a `systemd` / `launchd` service.

It is deliberately lightweight: Perseus depends on `athenaeum-core` with
`default-features = false`, so it pulls **none** of the image-rendering
(`rustafits`) or plate-solving (`solvemyastro`) machinery. Only header parsing,
the package format, the sync engine, and the iroh transport come along.

## What it does

1. **Watch** the capture directory for new `.fits` / `.fit` / `.fts` / `.xisf`
   files.
2. **Wait** until each file has finished being written — capture software writes
   FITS progressively, so a file is only picked up once its size and mtime hold
   steady for `stability_secs` (default 10 s).
3. **Package** the frame: parse its header into portable metadata, hash it, and
   wrap it in a self-describing bundle (one package per file).
4. **Send** it to the paired primary and record the peer's per-frame receipt.
   Delivery is durable — an unfinished transfer resumes after a crash or restart
   (state lives in `<data_dir>/perseus.db`).

Retention (deleting local frames after the peer confirms them) is **not** active
in this build. The config is parsed and validated, but the evaluator ships in a
later task; deletion stays disabled (`dry_run = true`, enforced) until then.

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
data_dir = "/var/lib/perseus"            # SQLite store + blob store + device key + logs
pairing_ticket = "<paste from primary>"  # primary → Settings → Sync (dev)
mode = "auto"                            # only value in the MVP

[retention]
policy = "keep_everything"               # keep_everything | on_confirm | keep_days | disk_pct
dry_run = true                           # MUST stay true (deletion is not implemented yet)

# Optional tuning (defaults shown; omit to accept them):
# stability_secs = 10                    # write-quiet window before a file is sent
# poll_interval_secs = 2                 # how often pending files are re-checked
```

Notes:

- **`pairing_ticket`** is the out-of-band pairing string from the primary. On
  first `run`, Perseus generates a device key at `<data_dir>/device_key` (mode
  `0600`) and derives its own node identity from it. Both keys persist, so the
  pairing survives restarts.
- **`dry_run = false` is rejected.** There is no deletion path yet, so the config
  refuses to imply one.
- The `[retention]` table may be omitted entirely — it defaults to
  `keep_everything` / `dry_run = true`.

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
journald / launchd log files). Raise the level without editing the config via
`ATHENAEUM_LOG` (full [`EnvFilter`] syntax, overrides the default `info`):

```bash
ATHENAEUM_LOG=info,perseus=debug perseus --config perseus.toml run
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
