# Stage 1.5 — owner manual verification runbook

Hands-on acceptance for the Stage 1.5 sync-hardening cycle (branch `0.4.0`,
commits `704850f3..db9467bf`). Companion to `scripts/sync_e2e_manual.md`
(M-Sync1): that runbook proves the Stage I *application* path; this one proves
the Stage 1.5 additions on top of it — blob GC, landing roots, the Perseus web
page, and history polish. Spec:
`docs/superpowers/specs/2026-07-08-stage15-sync-hardening-design.md`.

**Machines:** *primary* = the home Mac running the desktop app (signed in,
`primary` role); *capture* = the observatory box running Perseus. Everything
below assumes both are rebuilt from `0.4.0` at `db9467bf` or later.

**Time budget:** ~30 min active + one 15-minute wait (GC interval) + one
mid-week interruption test during the A9 soak.

---

## 0. Key paths cheat-sheet

| What | Primary (macOS app) | Capture (Perseus) |
| ---- | ------------------- | ----------------- |
| Data dir | `~/Library/Application Support/com.vsharifov.athenaeum` | `data_dir` from `perseus.toml` |
| Receiver blob store | `<data>/sync/blobs/sync_blobs` | — (Perseus never receives) |
| Sender blob store | `<data>/sync/blobs_out/sync_blobs` (new in 1.5) | `<data_dir>/sync_blobs` |
| Landed files (default) | `<data>/sync/incoming/<device>/<date>/` | — |
| Staging (new in 1.5) | `<data>/sync/staging/` | — |
| Logs | `<data>/logs/athenaeum-desktop.*` | `<data_dir>/logs/` |
| Sync DB | `athenaeum.db` (same dir) | `<data_dir>/perseus.db` |

Log level `info` (the default) is enough for every check below.

---

## 1. Build and restart both sides

Primary:

```bash
cd ~/Documents/Projects/athenaeum && git status   # expect branch 0.4.0, clean
npm run tauri dev                                  # or your usual build
```

Capture (observatory):

```bash
cd <repo> && git pull && git checkout 0.4.0
cargo build --release -p perseus
# restart however it runs (systemd/launchd/terminal), e.g.:
cargo run --release -p perseus -- --config ~/perseus-test/perseus.toml run
```

---

## 2. Check — startup sweep retires the accumulated leak (both sides)

Before restarting each side, note the blob dir size; after the restart, look
for the sweep line.

```bash
# BEFORE (capture example; use the primary paths from §0 on the Mac):
du -sh <data_dir>/sync_blobs

# AFTER restart, in the log:
grep "startup sweep" <logs>/perseus*.jsonl | tail -1
```

**PASS:** each side logs `blob store startup sweep removed stale tags` with
`count > 0` on the *first* 1.5 restart (that's the pre-1.5 backlog being
unpinned). Subsequent restarts log nothing (0 removed is silent).

**Note on disk:** the sweep unpins; bytes are reclaimed by the GC loop, which
runs every **15 minutes**. Re-run `du -sh` after ≥15 min — the size must have
dropped (on the primary, check both `blobs` and `blobs_out`). Don't judge disk
immediately after restart.

---

## 3. Check — sync incoming folder (primary)

1. App → **File Manager → Monitored Directories**. Two new sections exist:
   **Sync Incoming Folder** and **Collaboration Folder** (the latter is
   Stage-II-reserved — just confirm it renders).
2. Designate a Sync Incoming Folder. It must live **outside** any monitored
   directory — picking a nested one shows the friendly conflict message, not a
   raw error (try it once on purpose).
3. Trigger one send from the observatory (drop a new FITS into a capture dir).

**PASS:** the file lands under `<your folder>/<device-short>/<date>/…` (NOT in
app-data), the frame appears in the catalog, and no `.staging` dirs appear in
your folder (staging now lives in app-data).

**Fallback check (optional):** clear the designated folder, send again — the
file lands in `<data>/sync/incoming/...` and a one-time notification suggests
designating a folder. Re-designate afterwards: the *next* package follows the
new setting without an app restart (resolution is per-package).

---

## 4. Check — full send cycle releases blobs (both sides)

With everything running and paired, send one batch (a few files) and follow
the lifecycle in the logs:

```bash
# capture side:
grep -E "sync state|released" <logs>/perseus*.jsonl | tail -20
```

**PASS:**
- capture: `sync state … state="confirmed"` for the package;
- both sides: after confirm/ack, blob dirs stop growing; within one GC
  interval (≤15 min) `du -sh` on `<data_dir>/sync_blobs` (capture) and
  `<data>/sync/blobs*/…` (primary) returns to a small steady size;
- re-sending the same files → `Duplicate` receipts, zero new catalog rows
  (Stage I invariant, quick regression).

---

## 5. Check — Perseus web page

The page binds to `127.0.0.1:8686` by default (config: `web_bind`; empty
string disables; non-loopback bind requires `web_token`, otherwise Perseus
refuses to start — that refusal is deliberate).

From your Mac:

```bash
ssh -L 8686:127.0.0.1:8686 <user>@<observatory>
# then open http://127.0.0.1:8686 in a browser
```

Walk the four sections:

1. **Status** — capture dirs listed (all of them, see §6), in-flight table,
   retention policy + dry-run flag visible.
2. **Sent** — packages with state chips; confirmed rows have a checkbox +
   Delete. Note: totals are computed over the most recent 5000 rows.
3. **History** — search is **exact filename** match (labeled as such); rows
   show device *names* (not hex), duration + MB/s, and a green
   "safe to delete" chip on peer-accepted frames.
4. **Retention** — edit something harmless (e.g. `interval_secs`, or switch
   policy while `dry_run` stays on) → Save. Then on the observatory:

   ```bash
   grep -A2 '\[retention\]' <config>/perseus.toml
   ```

   **PASS:** the file shows the new value, your comments in the file are
   intact, `i_have_verified_the_soak` is untouched, and the log shows
   `retention config edited via web` — no restart. Trying `dry_run = false`
   from the web while the soak key is false must be REJECTED (422) with the
   file unchanged — try it once; this pins the safety gate.

5. **Manual delete** — pick ONE confirmed test package, Delete → confirm:

   ```bash
   # source file is gone from the capture dir, and the audit row exists:
   sqlite3 <data_dir>/perseus.db \
     "SELECT filename, outcome FROM sync_history WHERE outcome='deleted_manual';"
   ```

   Non-confirmed rows have no delete controls, and the API refuses them
   server-side regardless.

---

## 6. Check — multiple capture directories

In `perseus.toml`, switch to the array form (the old singular key still works;
setting both is a config error):

```toml
capture_dirs = ["/path/dir-a", "/path/dir-b"]
```

Restart Perseus, drop one new FITS into **each** dir.

**PASS:** the startup banner lists both dirs; both files are packaged, sent,
and confirmed (watch `status` or the web page's Sent section).

---

## 7. Check — history polish in the app (primary)

Open the **Transfers** panel (sidebar indicator) → History tab.

**PASS:** the peer column shows the device *name* from your hub account (hex
only when signed out — sign out briefly to see the fallback if you like);
finished rows show duration + MB/s; sent rows accepted by the primary carry
the "delivered — safe to delete" badge; failed/cancelled rows don't.

---

## 8. Check — interruption + resume (A5 tail; once, mid-soak)

During a real (or staged) multi-GB batch: kill the network on the observatory
mid-transfer (pull the cable / down the interface for ~2 min), then restore.

**PASS:** the sender retries/re-announces, the transfer completes and confirms
after reconnect, no duplicate catalog rows on the primary, and the capture
files are only eligible for retention *after* the confirm. This closes the
remaining piece of the A5 field-validation gate; record the result in the
A5 notes (`.superpowers/sdd/task-A5-report.md` flow).

---

## 9. If something looks wrong

- **Blob dir not shrinking:** wait a full 15-min GC interval first. Then check
  the sweep/release lines (§2/§4). A package stuck non-terminal (check
  `sqlite3 … "SELECT id,state,attempts FROM sync_outbound WHERE state NOT IN
  ('confirmed','failed');"`) legitimately pins its data.
- **Web page 401 loop:** the token prompt stores into browser localStorage;
  wrong token → re-prompt. Tokenless loopback via the SSH tunnel needs no
  token at all.
- **Web page missing:** bind conflict is non-fatal by design — Perseus logs
  `web status page failed to bind; continuing without it` and keeps syncing.
  Free the port (default 8686) and restart.
- **Files landing in app-data despite a designated folder:** check the log for
  `sync_incoming root lookup failed` (falls back on lookup errors) and confirm
  the folder still exists on disk.

## 10. Sign-off checklist

- [ ] Sweep line with count > 0 on first restart, both sides (§2)
- [ ] Blob dirs shrink within one GC interval and stay bounded (§2/§4)
- [ ] Files land under the designated sync-incoming folder; per-package
      re-resolution works (§3)
- [ ] Full cycle: confirmed → released → duplicate-safe re-send (§4)
- [ ] Web page: all four sections, retention edit preserved comments + soak
      keys, live apply, 422 on dry_run=false (§5)
- [ ] Manual delete: file gone + `deleted_manual` audit row; non-confirmed
      refused (§5)
- [ ] Both capture dirs feed the sync (§6)
- [ ] App history: names, duration, badge (§7)
- [ ] Mid-transfer interruption resumes and confirms (§8)

All boxes ticked → Stage 1.5 verified; the A9 soak continues on this build and
the remaining Stage I gates (M-Sync1 runbook, push `0.4.0`) proceed as planned.
