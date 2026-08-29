# M-Sync1 — two-instance sync proof (manual, real transport)

The **owner-run** acceptance procedure for the **M-Sync1** milestone (Stage I exit,
BRD Phase I). It exercises the same end-to-end path as the CI harness
(`crates/athenaeum-core/tests/sync_e2e.rs`, loopback), but over **real iroh + a
relay + hub account pairing**, on two physical machines — which is the only way
to prove NAT traversal, the relay map, and the hub device registry actually work
together.

There are **two variants**, both required for sign-off:

- **Variant A — Perseus auto-mode** (the observatory→home path): a headless
  Perseus capture agent auto-sends a 50-frame fixture run to the primary.
- **Variant B — full-app manual send** (the "Send to primary" path): a second
  full Athenaeum install, in the capture role, manually sends a selection.

> This runbook does **not** re-derive the raw iroh / NAT / relay validation — that
> is task A5's job. For the transport-level two-machine gate (10 GB transfer,
> kill/resume, cross-NAT, self-hosted relay) follow **`.superpowers/sdd/task-A5-report.md`
> → "OWNER: manual two-machine validation gate"**. This runbook assumes that gate
> already passed and layers the *application* proof (metadata, dedupe, retention,
> history, hub device list) on top.

---

## 0. Prerequisites

| Prereq | Where it's set up | Reference |
| ------ | ----------------- | --------- |
| Hub deployed + reachable | `athenaeum-hub` on `projects.artfrom.space` | `.superpowers/sdd/task-B1-report.md`, `task-b2-report.md` |
| `relay1` deployed + in the relay map | `athenaeum-hub` relay ops runbook | `.superpowers/sdd/task-B3-report.md` (server deploy is the owner follow-up documented there) |
| Account created (email-OTP) | app **Settings → Account**, or `perseus login` | `crates/perseus/README.md` |
| iroh transport validated | two-machine A5 gate passed | `.superpowers/sdd/task-A5-report.md` |
| rustc ≥ 1.91 on both machines | A5 report concern #1 (MSRV bump) | `.superpowers/sdd/task-A5-report.md` |

Two machines on **different networks** (both behind NAT) is the meaningful test;
LAN-only still proves the app layer but not relay traversal. The **primary** is
your home Athenaeum install (receiver, `primary` role). The **capture** side is
either a Perseus agent (Variant A) or a second full app in `capture` role
(Variant B).

**Fixture set.** Prefer 50 real light subs from one session (real-data-first).
Any 50 `.fits` / `.fit` / `.fts` / `.xisf` files work; keep them small if you want
a fast run. Have them staged **off** the capture directory so you can drop them in
as a controlled batch (Perseus only sends files that appear *after* it starts).

---

## Variant A — Perseus auto-mode (50-frame fixture run)

### A.1 Pair the capture machine

On the capture machine, install Perseus and write `perseus.toml` per
`crates/perseus/README.md` ("Install" + "Configure"). Use the **account** pairing
route (not a dev ticket) so this run proves the hub path:

```toml
# perseus.toml
capture_dir = "/data/capture"          # empty to start; you'll drop 50 files in
data_dir    = "/var/lib/perseus"
mode        = "auto"

[account]
hub_url = "https://projects.artfrom.space"
email   = "you@example.com"            # or omit and let `login` prompt

[retention]
policy  = "keep_everything"            # dry-run stays ON for M-Sync1 (see A.5)
dry_run = true
```

Sign in and register this node as a `capture` device paired to your primary:

```bash
perseus --config /path/to/perseus.toml login      # email → one-time code
perseus --config /path/to/perseus.toml status     # confirm pairing_route = account (hub …)
```

### A.2 Confirm the hub shows BOTH devices

On the **primary**, open **Settings → Account** → device list (or the hub's
`GET /devices` for the account). **Expected: two devices** — one `primary` (this
app) and one `capture` (the Perseus node), both non-revoked. This is the
"hub shows both devices" acceptance clause.

### A.3 Start the primary receiver

On the primary, launch Athenaeum. Ensure it is signed in and its role is
`primary` (**Settings → Account**). The receiver comes up automatically for a
signed-in primary; open **Transfers** (sidebar `TransferIndicator` → slide-over)
and leave it visible.

### A.4 Run the fixture batch

On the capture machine:

```bash
perseus --config /path/to/perseus.toml run        # watches; blocks until Ctrl-C
```

Wait for the "perseus online" log line, then **drop the 50 fixture files** into
`capture_dir` (e.g. `cp /staging/session/*.fits /data/capture/`). Perseus waits
`stability_secs` per file, packages each, and sends it.

Watch progress on both ends:

- **Capture (Perseus):** `journalctl -u perseus -f` (or stderr). Look for
  per-package `sync state` transitions ending in `confirmed`. Raise detail with
  `ATHENAEUM_LOG=info,perseus=debug`.
- **Primary (app):** the **Transfers** panel Active tab fills and drains; the
  History tab accumulates 50 received/ingested rows. A `sync` notification fires
  on completion.

### A.5 Verify — Variant A checklist

Let the batch fully drain (Perseus `status` shows `in-flight packages: 0`).

- [ ] **All 50 arrived with metadata.** On the **primary** catalog DB
      (`<app-data>/com.vsharifov.athenaeum/athenaeum.db`):

      ```sql
      SELECT COUNT(*) FROM frames;                         -- expect ≥ 50 new
      SELECT COUNT(*) FROM frames WHERE uuid IS NOT NULL;  -- every frame has its uuid
      SELECT object, exptime, filter, instrume FROM frames ORDER BY id DESC LIMIT 5;
      -- object/exposure/filter match the source subs (metadata survived the wire)
      ```

- [ ] **History complete on the receiver.** Primary DB:

      ```sql
      SELECT COUNT(*) FROM sync_history
       WHERE direction='received' AND outcome='ingested';   -- expect 50
      ```

- [ ] **History complete on the sender.** Perseus store
      (`<data_dir>/perseus.db`):

      ```sql
      SELECT COUNT(*) FROM sync_history
       WHERE direction='sent' AND outcome='ingested';        -- expect 50 confirmed
      SELECT state, COUNT(*) FROM sync_outbound GROUP BY state; -- all 'confirmed'
      ```

- [ ] **Interruption resumes** (proves durability): mid-run, kill Perseus
      (`Ctrl-C` or `systemctl stop perseus`) while packages are in flight, then
      restart `perseus … run`. The persisted `sync_outbound` rows re-drive; the
      batch still ends with all 50 `confirmed` and **no duplicate catalog rows**
      on the primary (re-check the counts above — they must not grow).

- [ ] **Re-run is dedupe-safe.** Stop Perseus, then
      `perseus … enqueue-backlog /data/capture` to resend the SAME 50 files. On
      the primary, `frames`/`files` counts **do not change**; the receiver logs 50
      `duplicate` rows:

      ```sql
      SELECT COUNT(*) FROM sync_history
       WHERE direction='received' AND outcome='duplicate';   -- expect 50
      ```

- [ ] **Dry-run retention log is correct** (retention stays dry-run for M-Sync1
      per the plan's hard invariant — live deletion is the separate M4/soak
      go-live). Set `policy = "on_confirm"` (leave `dry_run = true`), let a
      retention tick run, and confirm the log lists exactly the 50 confirmed
      sources as *would-delete* while **nothing is removed from disk**.

---

## Variant B — full-app manual send ("Send to primary")

A second **full Athenaeum** install in the `capture` role sends a selection by
hand — the M2b UI path, distinct from Perseus.

### B.1 Pair the second app as `capture`

On the second machine, install/launch Athenaeum, sign in to the **same account**
(**Settings → Account**), and set this machine's role to **`capture`**, pairing it
to your primary. **Settings → Account** device list on either machine must now
show the primary **and** this capture app (plus any Perseus node) — again the
"hub shows both devices" clause, this time for a full-app capture node.

### B.2 Manually send a selection

1. Scan a folder of frames into this capture app's catalog as usual.
2. Open the frames view (**Lights / Analysis**). On a signed-in capture node a
   **"Send to primary"** action appears on the selection toolbar (`useSyncSend`).
3. Select ~50 frames and **Send to primary**. Mixed/ineligible frames are
   reported back with reasons and the eligible remainder still sends
   (`(N of M)` convention).

Watch the primary's **Transfers** panel drain, exactly as in Variant A.

### B.3 Verify — Variant B checklist

- [ ] **50 arrived with metadata** — same primary-DB SQL as A.5 (frames count +
      uuid + sampled object/exptime).
- [ ] **Receiver history** = 50 `received/ingested` (primary DB).
- [ ] **Sender history** = 50 `sent/ingested` confirmed — on the **capture app's
      own catalog DB** (`sync_history` / `sync_outbound` live in the app catalog,
      not a separate `perseus.db`):

      ```sql
      SELECT COUNT(*) FROM sync_history WHERE direction='sent' AND outcome='ingested';
      SELECT state, COUNT(*) FROM sync_outbound GROUP BY state;   -- 'confirmed'
      ```

- [ ] **Re-run dedupe-safe** — re-send the same selection; primary `frames`/`files`
      counts unchanged; second ack is all `duplicate`
      (`… direction='sent' AND outcome='duplicate'` = 50 on the sender;
      `… direction='received' AND outcome='duplicate'` = 50 on the primary).

- [ ] **A send deletes nothing.** Owner ruling 2026-08-29: retention is a
      Perseus-only concern — the full-app retention loop and its
      `sync.retention.*` settings were removed, so there is no policy to set and
      nothing to enable (Settings → Sync never had that UI). After the two
      confirmed deliveries above, on the capture app's own catalog DB confirm the
      catalog is intact and no deletion was ever audited:

      ```sql
      SELECT COUNT(*) FROM sync_history WHERE outcome='retention_deleted';  -- 0
      SELECT COUNT(*) FROM files;    -- unchanged: the 50 sent + any never-synced file
      SELECT COUNT(*) FROM frames;   -- likewise unchanged
      SELECT COUNT(*) FROM sync_history WHERE direction='sent' AND outcome='ingested';
      -- 50: transfer events are the ONLY history a send writes
      ```

      Confirm on disk that all 50 source files are **still there**, alongside any
      never-synced file in the same catalog. The app never deletes a sent source;
      reclaiming disk on a capture node is Perseus's job (Variant A).

---

## Sign-off

M-Sync1 passes when **every checkbox in both variants** is ticked, on real
transport across two networks, with the hub showing both devices. Record the run:

- **Results doc:** copy the template below into
  `docs/superpowers/research/2026-07-XX-msync1-run.md` (date the filename), fill
  the observed counts, and note machines / networks / relay used.
- **Roadmap:** the engineering checkboxes (Track A / Track B / Merge) are already
  ticked in `docs/superpowers/plans/2026-07-02-roadmap.md`; on sign-off, record
  **M-Sync1** as achieved in the milestone table there (owner action).

### Results-doc template

```markdown
# M-Sync1 run — <YYYY-MM-DD>

- Primary machine / network: …
- Capture machine / network: …  (behind NAT: yes/no)
- Relay used: relay1.artfrom.space / self-hosted / default
- Hub: projects.artfrom.space   Account: …
- Builds: app <version/commit>, perseus <commit>, rustc <ver>

## Hub device list
- [ ] primary + capture both listed, non-revoked        devices: …

## Variant A — Perseus auto-mode (50 frames)
- [ ] 50 frames on primary with uuid + metadata          frames=…
- [ ] receiver history 50 received/ingested              …
- [ ] sender history 50 sent/ingested (all confirmed)    …
- [ ] interruption resumed, no dupes                     …
- [ ] re-run dedupe-safe (50 duplicate, counts stable)   …
- [ ] dry-run retention log correct, nothing deleted     …

## Variant B — full-app manual send (50 frames)
- [ ] 50 frames on primary with uuid + metadata          frames=…
- [ ] receiver / sender history complete                 …
- [ ] re-run dedupe-safe                                 …
- [ ] a send deletes nothing: retention_deleted=0, files/frames unchanged
- [ ] all 50 sources + the keeper still on disk          …

## Verdict
PASS / FAIL — notes:
```

---

## What the CI harness already proves (so you don't have to)

`crates/athenaeum-core/tests/sync_e2e.rs` runs the identical scenario shape over
the in-process loopback transport on every `cargo test -p athenaeum-core`: two
`ServiceContext`s, 50 fixture frames, full-metadata ingest, dedupe-safe re-run
(all-`Duplicate` acks), the proof that **a send deletes nothing** (every source
and the never-synced keeper survive, `retention_deleted` = 0), and history
complete on both ends — all asserted via SQL. The
manual run above adds only what loopback cannot: real iroh, the relay, NAT
traversal, and the hub device registry.
