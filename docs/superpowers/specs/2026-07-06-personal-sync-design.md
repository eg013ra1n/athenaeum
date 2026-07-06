# Personal Sync (Stage I) — Design — 2026-07-06

**Status:** Approved in brainstorm 2026-07-06, pending owner review of the written spec. **Owner:** Vilen.
**Inputs:** BRD `2026-07-05-sync-collaboration-brd.md` (section B + A1–A3), components doc `2026-07-05-sync-collaboration-components.md` (C-1, C-3, C-4, C-5, C-10). This spec **closes D-1 and D-2** and replaces the S-1/S-2 spike plan with a Perseus-first build. Task labels (Wn/In) cross-reference the superseded `../plans/2026-07-05-sync-stage0-stage1-tasks.md` for traceability. Package/manifest format (§5) is shared with Stage II (`2026-07-06-collaboration-projects-design.md`).

---

## 1. Scope

BRD Phase I: one account, observatory→home flow, retention, transfer history, Perseus capture agent. Out of scope: projects/portal/quality gate (Stage II spec), bandwidth scheduling (B10, Phase IV), change journal (not needed for one-way personal sync).

Already shipped (roadmap Phase 1, v0.2.4+): catalog/frame UUIDs + `catalog_meta`, `updated_at` triggers, ts-rs contract, shared `core::api` command layer, FITS writer. The 2026-07-05 task plan's W1/W2 are therefore **done** and drop out of Stage 0.

## 2. Decisions closed here

### D-1 Transport = iroh (embedded)

Rationale (researched 2026-07-06):

- **Both-sides-behind-NAT** (the normal case: observatory on LTE/CGNAT, home behind router): iroh hole-punches ~90% of connections direct (Tailscale-model, relay-assisted), falls back seamlessly to relays over HTTPS/443. Syncthing punches less reliably and its relay protocol is a separate TCP port by default.
- **Relay coverage**: Syncthing's free community pool is large but volunteer-bandwidth (per-session throttles — painful for nightly tens of GB). iroh's n0 public relays are dev-only → production requires self-hosted relays, which in this product is not a cost but the monetization asset (BRD Q2: paid private relays).
- **Self-hosted / paid relay**: `iroh-relay` is a single Rust binary, MIT/Apache-2.0, with **built-in HTTP-callback access control** (`access.http.url`) — the hub authorizes each connecting node id. "Private relay per project" becomes a stock mechanism, no fork. Syncthing's `strelaysrv` has only a static per-relay token; per-account gating would need a fork under MPL obligations.
- **Perseus**: embedding a Rust library in a small headless ARM binary is trivial; managing a Syncthing sidecar is a subsystem.

Cost accepted: we operate relay infrastructure from day one. Fallback: everything above the `SharingTransport` trait (C-4) stays transport-agnostic; if iroh validation fails (§11 track A gate), Syncthing sidecar is the recovery path.

### D-2 Auth = passwordless email OTP + optional Discord OAuth link (Stage II)

- Base sign-in everywhere (app, Perseus, portal): enter email → 6-digit code by mail → enter code. Fully in-app, no browser redirects, works on a headless agent's config page. First successful OTP creates the account (email = account).
- No passwords stored, ever. Only new dependency: one transactional email provider (Resend/Postmark/SES class; free tier suffices initially).
- Discord identity is **not** part of base auth: linked later via OAuth (`identify` scope) only when a user joins collaboration features (Stage II/III). By Phase III the profile's Discord nick is OAuth-verified, so the bot invites real accounts, not typed nicks.
- Tokens: hub issues one **opaque token per device**, stored hashed in Postgres. Device revocation (A1) = row delete, no JWT invalidation acrobatics. Client stores the token in the OS keychain (macOS Keychain / Windows Credential Manager / Secret Service); Perseus: Secret Service when available, else `0600` file.
- Device identity (D-5 unchanged): keypair generated at first sign-in; the public key is both the hub device id and the iroh node id.
- Abuse hygiene: OTP rate-limited per email and per IP; codes TTL 10 min, single-use.

The S-2 spike collapses into implementation tasks (email provider smoke test, keychain storage on three OSes).

## 3. Architecture overview

Two parallel tracks that merge; Perseus is the MVP vehicle for validating iroh in production conditions (owner's real observatory), not a late add-on.

```
Track A (transport + engine + Perseus)          Track B (service)
  C-4 trait + LoopbackTransport                   hub skeleton (accounts/devices/relay-map)
  package/manifest v1 (§5)                        email-OTP auth + device registry
  sync engine library (§6)                        relay1.artfrom.space (iroh-relay + HTTP-auth)
  iroh impl (§4; first days = validation)         Settings → Account UI (app)
  Perseus MVP (§7, ticket pairing, dry-run)
        └──────────────► merge: account pairing replaces tickets, Transfers UI,
                         manual mode, retention goes live ══ M-Sync1 (BRD Phase I)
```

## 4. Transport: `SharingTransport` over iroh (C-4)

**Trait surface** (conceptual; final signatures in the implementation plan):

- `announce(package_id, to_node) → ()` — small control message: "package X available".
- Provider side: serve package blobs to authorized fetchers (pull model — receiver fetches; fits iroh-blobs natively and generalizes to the Stage II swarm).
- `fetch(package_id, from_node) → TransferHandle` — verified, resumable download.
- `ack(package_id, frame_receipts) → ()` — receiver→sender confirmation after ingest (frame uuid + content hash each).
- Progress events → existing `ProgressEmitter`; resume-after-restart enumeration; relay-map injection (from hub; static file fallback for the dev flag).

**iroh mapping**: one `Endpoint` per process (device keypair = node id); `iroh-blobs` for content-addressed transfer — per-frame files are blobs, a package is a hash-seq collection (§5); BLAKE3 verification is inherent to the transfer; a tiny custom protocol (single QUIC stream) carries announce/ack messages. Both peers must be online simultaneously for a transfer; the capture node retains files until confirmed (B4), so intermittent overlap only delays, never loses.

**Relays**: `relay1.artfrom.space` (separate hostname from day one — moving it off the shared VPS later is a DNS change). iroh-relay with HTTP-callback auth pointed at a hub endpoint that checks the node id against registered devices. During the MVP dev-flag phase, n0 public (dev) relays are acceptable.

**Validation (folds the old S-1 spike into track A's first days)**: before the impl hardens, script the B4/B5 scenario on raw iroh APIs — ~10 GB FITS, kill network mid-transfer, resume, hash-confirmed delivery, delete-at-source only after confirmation; then the same through self-hosted iroh-relay with both peers NATed. Failure of this gate = fall back to Syncthing behind the same trait.

**`LoopbackTransport` mock** (in-process, fault-injectable: mid-transfer abort, duplicate delivery, delayed/lost ack) ships with the trait so the engine (§6) is fully testable without a network.

## 5. Package / manifest format v1 (shared with Stage II)

`crates/athenaeum-core/src/package/`.

- **Blob = one payload file** (one FITS/XISF). **Package = manifest + a set of blobs**, transferred as an iroh hash-seq collection. Content addressing gives dedupe (re-announcing a batch with 90% old frames transfers only new blobs) and per-blob resume.
- **`manifest.ndjson`**, `"v": 1`, one record per blob:
  - identity: frame `uuid`, origin `catalog_uuid`, origin device;
  - `payload_kind`: `raw_frame | calibrated_light | master | other` — `raw_frame` covers any raw capture (light/dark/flat/bias; IMAGETYP travels in the metadata snapshot). Stage I personal sync ships `raw_frame`; Stage II publishes `calibrated_light`. The field exists from v1 so a future per-project "raw+masters" mode is a value, not a format break;
  - file: rel path, byte size, **xxh3 content hash** (catalog convention; transport-level BLAKE3 comes free from iroh-blobs — both recorded);
  - metadata: full frames-row snapshot + analysis summary when present (B8 — metadata travels with files);
  - versions: app/engine version stamps.
- Supersede identity: same frame `uuid`, different content hash = a newer version of that frame's payload (used by Stage II re-publications; harmless in Stage I).
- Writer from a frame/file selection; reader + validation; optional zip container via existing `zip_writer` for offline export (the transport uses loose blobs, not zips).

## 6. Sync engine library (C-5, D-7)

`crates/athenaeum-core/src/sync/` — **built as a library layer decoupled from the catalog** behind a small storage trait (`SyncStore`): the full app implements it over the catalog DB; Perseus over its own standalone SQLite file. Queue worker: in the app — the existing `operation_queue` (`OperationKind::SyncTransfer`); in Perseus — a plain worker thread over the same engine.

**Build constraint (bites if ignored):** Perseus must build *without* rustafits/turbojpeg native deps (nasm/cmake on ARM). Mechanism: feature-gate `athenaeum-core` so render/analysis/plate-solve modules are behind a default-on feature and Perseus builds `--no-default-features --features sync` (exact gating in the implementation plan).

**Sender state machine** per package: `queued → announced → transferring → delivered → confirmed | failed(attempts)`. Crash-resume: on startup re-enumerate non-terminal states and re-announce/re-fetch. Duplicate acks are idempotent.

**Tables** (in whichever `SyncStore` backs the node):

- `sync_outbound (id, package_ref, peer_device, state, attempts, created_at, confirmed_at)`
- `sync_history (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome)` — append-only both directions, indexed by filename/object/date (B6), fed to hub as **aggregate stats only**.

**Retention service** (capture node; `sync/retention.rs`): policies per BRD B5 — delete-on-confirm / keep N days / disk ≥ X% oldest-confirmed-first. Hard invariants enforced in one place and unit-tested: only `confirmed` files are ever deleted; deletion goes through the existing `file_op` pipeline in the app shell (catalog-consistent) or a direct-but-logged path in Perseus; untransferred files untouchable regardless of disk pressure (nothing eligible → warn notification instead). **Dry-run mode is the default until M-Perseus-MVP soak passes**: the evaluator logs what it *would* delete; real deletion is a config opt-in.

**Auto & manual modes** (B2/B3): auto — the scanner/monitor "scan finished" path enqueues newly ingested files (per-node toggle, all frame types); manual — `enqueue_sync_selection` command + "Send to primary" selection action (full-app capture nodes only; Perseus is auto-only per BRD B1a).

## 7. Perseus — capture-node agent (C-10)

New workspace crate `crates/perseus/` (workspace keeps the library boundary honest while it stabilizes; can be extracted later).

**MVP (track A exit, dev-flagged):**

- Config file (TOML): capture directory, **pairing ticket** from the primary, mode (auto), retention policy (default: keep-everything + dry-run).
- Ticket pairing: the primary app's Settings → Sync shows a copyable iroh node ticket; no hub, no account. Explicitly a dev/MVP mode behind a flag — BRD A1/B1 bind sync to accounts and that stays the product path; whether tickets survive as an offline fallback is deliberately deferred.
- Watcher (`notify`-based, monitor pattern) → header parse via `fits_parser` (manifest metadata; no analysis fields) → package → announce/serve → confirmed → history + (dry-run) retention.
- Runs as a service (systemd/launchd); build targets include Linux ARM64. Logging: `tracing` per repo conventions, own JSONL file prefix.

**Product phase (post-merge):** email-OTP sign-in (I1 client subset), device registration as capture node, target-primary selection from the account's device list, relay-map from hub, minimal local web status page (Syncthing-style, bind localhost). No catalog DB, no library UI — ever.

**M-Perseus-MVP milestone:** the owner's observatory runs Perseus in auto mode for ≥1 week of real sessions: everything captured arrives at the home primary with metadata, interruptions resume, re-runs dedupe (zero duplicate catalog rows), dry-run retention log is correct, history complete on both ends.

## 8. Hub minimal + app account layer (C-1 subset, C-3)

**Hub** (new repo `athenaeum-hub`, Axum + Postgres, single deployable, D-3/D-4): Stage-I endpoints only —

- `POST /auth/otp` {email}; `POST /auth/verify` {email, code, device_pubkey, device_name} → device token (opaque, hashed at rest);
- devices: list / rename / revoke; role registration (primary | capture) with **exactly-one-primary per account** enforced hub-side;
- `GET /relay-map` (per-account; static list initially; later per-project for paid relays);
- relay auth callback endpoint for iroh-relay's `access.http.url` (checks node id ∈ registered devices);
- aggregate stats ingest stub (A4 comes in Stage III).

Deployed to the existing VPS behind nginx (`projects.artfrom.space/api/v1/…` — one origin shared with the Stage II portal; the hostname ships in Stage I even though the portal doesn't).

**App account layer** (`crates/athenaeum-core/src/account/`): hub client (reqwest), token in keychain, `Settings → Account` UI (sign-in, device list, revoke, machine role + peer selection — I2). Signed-out state hides sync surfaces only; **A2 test: every pre-existing page renders signed-out**. All new commands via the shared `core::api` layer with web mirrors.

## 9. Primary-side ingest & ack (I6)

Receive package → hashes already BLAKE3-verified by transport; re-verify xxh3 against manifest → land files under a per-capture-node "incoming" folder inside a configured scan root (template-organized) → apply manifest metadata → **dedupe by frame uuid, then content hash** (same frame twice = one catalog row, B7) → **primary-wins merge** (B9: manifest metadata never overwrites a newer `updated_at` on primary) → ack (frame uuid + hash per frame) → sender flips `confirmed`. Origin device recorded on history rows. Ack is idempotent; ack loss → sender re-announces, receiver replies from its receipt log without re-ingesting.

## 10. Transfers UI & notifications (I8)

- **TransferIndicator** in the sidebar (next to `ComputeQueueIndicator`): aggregate ↑/↓ rate + queue depth; click opens **Transfers**.
- **Transfers** surface (slide-over/page, NotificationPanel machinery): active queue (per-item state/progress/speed via existing event channels) + searchable history (B6). One surface for personal sync now and Stage II project exchange later (same engine tables + a project dimension then).
- Notifications: new `NotificationKind: 'sync'` (union + icon map per convention), discrete outcomes only — "N frames arrived from <device>", "transfer failed", "retention blocked: disk full but nothing eligible". Never per-progress.

## 11. Sequencing, milestones, risks

**Track A** (start immediately): trait + Loopback → package v1 → engine core → iroh validation days (gate: B4/B5 scenario passes; else Syncthing fallback) → iroh impl → Perseus MVP → primary ingest → **M-Perseus-MVP** (soak on the real observatory).
**Track B** (parallel): hub skeleton + OTP auth → deploy → relay1 with HTTP-auth → app account layer + Settings UI.
**Merge**: account pairing replaces tickets (dev flag stays for tests) → machine roles (I2) → manual mode → Transfers UI → retention live (dry-run off) → two-instance E2E harness (loopback in CI; real transport manually) → **M-Sync1** = BRD Phase I exit: Perseus auto-mode 50-frame fixture run + full-app manual-mode variant, dedupe-safe re-runs, policy deletion with two history events, hub shows both devices.

**Risks**: retention deleting real data (mitigated: dry-run default, invariants unit-tested, `file_op` pipeline reuse); iroh API churn at 1.0 (pin version; trait isolates); relay ops (start on existing VPS, separate hostname for painless move); feature-gating core for Perseus turns up hidden couplings (schedule as an early track-A task, not a late surprise).

## 12. Explicitly deferred

Store-and-forward relays (Phase IV, needs BRD amendment), bandwidth scheduling (B10), ticket-pairing as a permanent offline mode (decide post-MVP), change journal (Stage II re-import needs it, not one-way sync), Windows service packaging for Perseus (macOS/Linux first; Windows in the product phase).
