# Sync Model — Phase 1 Design — 2026-07-11

**Status:** design (brainstormed with owner 2026-07-11). **Repos touched:** `athenaeum` (core + tauri + web), `athenaeum-hub`, `perseus`.
**Builds on:** Stage I personal sync (`2026-07-06-personal-sync-design.md`) and the 2026-07-11 sync security fixes (C1 package-id path guard, H1 receiver allow-list, M3 confirm gate). **Reuses** the iroh transport, package/manifest format, sync engine, and hub account layer unchanged except where stated.

This is **Phase 1 of 3**. Phases 2 (Perseus send-queue UX) and 3 (app→app file-browser send UI) are separate spec → plan → implementation cycles and are out of scope here (see §11).

---

## 1. Goals

Evolve personal sync from a fixed **one-primary star** into an **explicit multi-node model** within a single account:

1. **Any Athenaeum node is a full peer** — it both receives and sends. **Perseus is send-only.** No "one primary per account" limit; no fixed capture→primary pairing.
2. **The sender always chooses the destination explicitly** from the list of the account's Athenaeum nodes (Perseus: configured targets; app: ad-hoc, Phase 3).
3. **Directory structure is replicated** — the receiver mirrors the sender's folder tree instead of the flat `<node>/<date>/<file>` layout.
4. **Custom, unique node names** — default to hostname on every OS, editable in settings.
5. **Pre-transfer dedup** — never re-transfer a file the receiver already has; the hub stays data-free (the exchange is peer-to-peer).

Non-goals (Phase 1): two-way mirroring, delete/rename propagation to the receiver, raw (non-catalog) file sync, cross-account collaboration (that is the separate Stage II collaboration design).

---

## 2. Resolved decisions (owner, 2026-07-11)

- Topology: **my own account, multiple receivers.** Athenaeum = always full peer; Perseus = send-only.
- Landing layout: **`incoming_root/<sender-node-slug>/<mirrored tree>`** (source-prefixed mirror).
- Node names: **unique per account** (hub-enforced); default hostname cross-OS.
- Dedup: **P2P offer/want at send time**, sampling-hash pre-filter + full-hash confirm on match; **hub stays data-free** (no per-node hash index on the hub).
- Targeting: **explicit** — drop `peer_device_id` / default-target entirely.

---

## 3. Roles → capability (hub)

Replace the `role ∈ {primary, capture}` + `one_primary_per_account` model with a **device capability**:

- `athenaeum` — a full peer: can receive (runs a `SyncReceiver`) and send.
- `perseus` — send-only: never runs a receiver, never a valid destination.

**Hub schema (migration `0007_capability`):**

- Add `devices.capability text NOT NULL DEFAULT 'athenaeum'` (values `athenaeum` | `perseus`).
- Drop the `one_primary_per_account` partial-unique index.
- Backfill: `capability = 'athenaeum'` for existing app devices (incl. the current primary), `'perseus'` for Perseus devices (detected by the existing role/registration signal). Drop the now-unused `role` column read paths (keep the column nullable for one release to avoid a hard break, stop writing it).
- `peer_device_id` is no longer written or read (targets are explicit). Keep the column for one release, stop populating it; the revoke-time peer-clear logic can be dropped.

**Hub API:**

- `POST /auth/verify` sets `capability` from a new request field `deviceCapability` (`athenaeum` default; Perseus sends `perseus`).
- `GET /devices` returns `capability`. The client's "destination list" = devices where `capability = 'athenaeum'` and `id != self` and `revoked_at IS NULL`.
- Role endpoint (`POST /devices/{id}/role`) is removed or repurposed; capability is set at registration and is not a user toggle in v1.

**Client:** `DeviceRole` enum → `DeviceCapability`; `AccountDevice.capability`; the app registers as `athenaeum`, Perseus as `perseus`.

---

## 4. Receiver authorization (updates H1)

The H1 allow-list was "capture devices paired to this primary." With no pairing, it becomes: **any non-revoked device in my account.** They are all mine and mutually trusted.

- `refresh_authorized_peers` (in `api::sync`) fetches `GET /devices` and caches **every** account device's pubkey (minus self) into `SYNC_AUTHORIZED_PEERS`, replacing the "capture paired to me" filter.
- The receiver's `PeerAuthorizer` still enforces the cached set (unchanged mechanism; the H1 security property holds — a non-account node is rejected before any fetch).
- Fail-closed unchanged: an empty cache authorizes nobody until the first hub refresh.

---

## 5. Node names (req #3)

**Source of truth:** `devices.name` on the hub, **unique per account** (a `UNIQUE (account_id, lower(name))` index; a colliding rename → `409`, the UI proposes an auto-suffix).

**Default:** the machine hostname on **every** OS (Perseus + Athenaeum), read via a cross-platform hostname call. Empty/unavailable → `perseus-<short-id>` / `athenaeum-<short-id>` (short id = first 6 hex of node id).

**Editing:** new hub endpoint `PATCH /devices/{id}` accepting `{ name }` (account-scoped, same auth). Surfaced in Perseus web (Account/Settings card) and Athenaeum `Settings → Sync/Account`.

**Path safety (load-bearing — the name becomes a directory component):**

- The receiver builds the folder prefix from the name **it resolves for the sender's node id** (from its cached device list / pairing cache) — **never** from a name embedded in the incoming package. This keeps the prefix under the receiver/account's control, not attacker-influenced.
- That resolved name is reduced to a filesystem **slug**: lowercase, `[a-z0-9._-]` only (others → `-`), non-empty, not `.`/`..`, length-capped, collision-suffixed. Reuses the sanitizer discipline from the C1/L1 fixes; a name that resolves to nothing safe falls back to the node-id short hex.

Renames are additive: new packages land under the new slug; existing folders keep their old slug.

---

## 6. Directory-structure replication (req #1)

**Sender — "sync-root".** Each source is anchored to a sync-root; `manifest.rel_path` becomes the file's path **relative to that sync-root** (today it is just the basename):

- **Perseus:** each configured `capture_dir` is a sync-root. `rel_path` = the file path relative to its `capture_dir` (e.g. `M31/2026-07-10/lights/L_0001.fits`).
- **App (Phase 3 UI, mechanics defined here):** a picked folder is the sync-root (`rel_path` relative to it); individually-picked files land flat (`rel_path` = basename).

`rel_path` stays forward-slash and continues to pass `validate_rel_path` (no `..`/root/backslash/drive) — already enforced on both write and receive sides.

**Receiver — landing.** `land_payload` changes from `incoming_root/<device_short>/<date>/<basename>` to:

```text
<incoming_root>/<sender-node-slug>/<rel_path>
```

creating intermediate directories, then catalog-ingesting exactly as today (files/frames/fits_header rows, uuid/content dedup). One-way, additive. Collisions (same landing path, different content) keep the existing `_2`/`_3` collision-suffix.

**Multi-root edge case (Perseus, >1 `capture_dir`):** two capture_dirs can yield the same `rel_path`, colliding under one node slug. Resolution: prefix `rel_path` with a per-root label (the sanitized `capture_dir` basename) when the node watches more than one capture_dir. Single-root senders get no extra prefix. (Recorded so the mirror stays lossless; final label scheme pinned in the plan.)

**Eligibility unchanged:** only FITS/XISF payloads are packaged and mirrored (Perseus watcher's existing extension filter).

---

## 7. Pre-transfer dedup — offer/want handshake (P2P)

A new control exchange on the existing `athenaeum/sync/1` ALPN, **before** the current `Announce`. The hub is not involved (stays data-free). Correctness does not depend on it — the receiver's ingest-time uuid/content dedup remains the safety net; this is purely a bandwidth optimization and may be best-effort.

**Messages (postcard, additive to `Msg`):**

1. `Offer { package_id, entries: [{ rel_path, sampling_hash, byte_size }] }` — sampling_hash = the existing `duplicates::compute_xxhash` (first/middle/last 512 KB), which the receiver already indexes for every catalog file in `files.content_hash`.
2. `Want { package_id, want: [rel_path], candidates: [rel_path] }` — receiver split:
   - no sampling match in `files.content_hash` → **want** (definitely absent);
   - sampling match → **candidate** (possible duplicate — must confirm, because the sampling hash can false-positive and a blind skip would silently drop a genuinely-new file).
3. For candidates only: `FullHashes { package_id, entries: [{ rel_path, xxh3_full }] }` (sender) → receiver re-hashes its own matching file(s) with full `xxh3` and confirms; a real match is dropped, a false positive is added back to `want`. Final want set is the intersection to transfer.

The sender then builds/serves a package containing **only** the final want set and proceeds with the existing `Announce → fetch → ingest → ack` flow. The batch outcome (`N new, M duplicate`) is returned to the caller for the approve/history surfaces (Phase 2).

**Why sampling→full and not a full library index:** the sampling hash already covers the whole library at zero extra cost; the full hash is read only for the handful of sampling-collision candidates (usually the one real duplicate), so we get full coverage *and* exactness without a new full-hash pass or migration.

---

## 8. Target selection

Sends are always addressed to explicit destination node(s) — no default target.

- **Perseus:** a `targets` list in `perseus.toml` (node names or ids), editable on the web page; Perseus sends each batch to each configured target. Resolution: name/id → node id via the cached account device list.
- **App → app:** ad-hoc at send time (pick files/folder → pick destination node from the account's `athenaeum` list). The selection→queue mechanics live here; the file-browser UI is Phase 3.
- Transport unchanged: the sender resolves the destination node id and dials it (direct or relay) over the existing iroh endpoint.

---

## 9. Data model & interfaces summary

**Hub:** migration `0007` (add `capability`, drop `one_primary_per_account`, add `UNIQUE(account_id, lower(name))`); `deviceCapability` on verify; `capability` on `GET /devices`; new `PATCH /devices/{id}` (name). Tests in `athenaeum-hub/tests/`.

**Core (`athenaeum-core`):**

- `manifest.rel_path` populated relative to the sync-root (writer/`api::sync`/Perseus).
- `sync::ingest::land_payload` → `<incoming_root>/<sender-slug>/<rel_path>`.
- Sender-slug resolver (receiver-side, from cached device names → sanitized slug).
- New `Msg::Offer/Want/FullHashes` in `sharing::iroh::proto` + engine/receiver handling; sender builds the package from the want subset.
- `refresh_authorized_peers` → all account devices.
- `DeviceRole` → `DeviceCapability`.

**Clients:** Perseus `targets` + name editing + cross-OS hostname default; Athenaeum name editing + capability registration + destination picker plumbing.

---

## 10. Security considerations

- **Path safety:** the sender slug (§5) and `rel_path` (§6) are the two attacker-influenced path components. `rel_path` is already `validate_rel_path`-guarded (C1/L1); the slug is receiver-resolved (not sender-supplied) and sanitized. No new traversal surface.
- **Authorization:** the H1 allow-list widens to "account devices" but keeps its fail-closed enforcement (non-account nodes rejected pre-fetch). Perseus (`perseus` capability) is never a valid destination.
- **Hub privacy invariant preserved:** the dedup exchange is P2P; the hub never sees file hashes, names, or batch composition — only identity/registry (as documented in the sync architecture doc).
- **Dedup can't cause loss:** sampling-match candidates are confirmed by full hash before skipping; and ingest-time dedup is the final safety net.

---

## 11. Out of scope — follow-up phases

- **Phase 2 (req #4 — Perseus send workflow):** watch → pending-set with per-file status; drop files removed-before-send; hierarchy (tree) UI of "to sync"; manual/auto toggle; manual = build queue → approve → send as a batch; auto = debounced batches (never per-file); history in batches, auto-sends grouped by calendar day. Consumes the §7 offer/want outcome for the approve step.
- **Phase 3 (app→app send UI):** file-browser selection of files/folder + destination node picker + outbound queue, reusing §6/§7/§8 mechanics.

---

## 12. Testing strategy

TDD throughout, project gates as usual (workspace build, core tests, `tsc`, hub suite against a throwaway Postgres). Key new tests:

- **Hub:** capability set on verify; `GET /devices` filters destinations; duplicate name → 409; `PATCH` renames; migration backfill.
- **Core:** mirror landing preserves the tree under `<slug>/<rel_path>`; slug sanitizer rejects path-unsafe resolved names; offer/want skips a true duplicate and transfers a sampling-collision false-positive; allow-list = account (accept account peer, drop non-account); multi-root Perseus collision handled.

---

## 13. Client design decisions (confirmed with owner 2026-07-11)

Resolving the client half of §3–§6, which the model sections left open. The hub half (Plan 1) is already merged (`athenaeum-hub` main, not yet deployed).

1. **Receiver autostart = signed-in, no role gate.** Every signed-in Athenaeum node runs a `SyncReceiver` (it is always an `athenaeum` capability = full peer). The old `account_primary_ready` / `role == Primary` gate on `autostart_if_enabled` is replaced by a plain "signed in (account identity present)" check. Perseus never receives.
2. **The app no longer auto-sends; sending is explicit.** In the mesh model the app has no fixed primary to push to, so the capture-on-scan auto-enqueue and the whole `resolve_capture_peer` / paired-primary / `ACCOUNT_PEER_DEVICE_ID` send machinery is **removed from the app**. The app sends only via an explicit user action (pick files/folder + pick destination node) — that UI is Phase 3. Auto-send survives **only in Perseus**, to its configured target node(s) (Plan 2C / Phase 2). The scan-completion auto-enqueue hook and `SyncSenderRuntime`'s single-peer resolution are re-pointed to the explicit-target model, not role/pairing.
3. **Account UI drops the role selector.** `AccountSection.tsx` no longer offers a primary/capture toggle or `set_role`. It shows the device's **capability** (fixed, informational — `athenaeum`), an **editable name** (→ hub `PATCH /devices/{id}`), and the account device list (with capability + name). `RoleBadge`/`applyRole`/`handleSelectRole`/the role radios are removed; `useAccount.setRole` is removed; `DeviceRole` in `models.ts` → `DeviceCapability`.
4. **Old local state is inert.** `ACCOUNT_ROLE` and `ACCOUNT_PEER_DEVICE_ID` settings are no longer read or written; on upgrade they are simply ignored (a best-effort clear is optional, not required — nothing reads them).

**Implementation slices (each its own plan → SDD cycle):**
- **Plan 2A — core: capability model + account-wide allow-list.** `DeviceRole` → `DeviceCapability` (core `account` + `api::account` + `api::sync` + `sync::status`/`pairing`); the hub client sends `deviceCapability` on verify and reads `capability` from `/devices`; `refresh_authorized_peers` caches **every** account device; the receiver autostart gate becomes signed-in (decision 1); remove `set_role` from the client + its tauri/web command/route + `ACCOUNT_ROLE`/`ACCOUNT_PEER_DEVICE_ID` reads.
- **Plan 2B — core: mirror landing** (§6): `rel_path` relative to a sync-root, `land_payload` → `incoming_root/<sender-slug>/<rel_path>`, receiver-resolved sanitized slug.
- **Plan 2C — explicit-target send path + client UI/naming**: `enqueue`/`SyncSenderRuntime` take an explicit destination; remove app auto-send (decision 2); cross-OS hostname default + name-editing UI (Perseus web + `AccountSection.tsx`, decision 3) + `PATCH` wiring; Perseus `targets` config + `rel_path` from `capture_dir`.

Plan 2 (all three slices) ships **together with the hub deploy** so the removed role/pairing contract never breaks a live client.
