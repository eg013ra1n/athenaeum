# Sync & Collaboration — Component Architecture (Draft) — 2026-07-05

Engineering decomposition of `2026-07-05-sync-collaboration-brd.md`. Draft for owner review; open decisions are tagged **D-n** and mapped to BRD open questions. Written while Phase 2 (calibration/masters) is in active development — the "start now / must wait" split at the end respects that.

> **2026-07-06 update:** the design round produced `2026-07-06-personal-sync-design.md` (Stage I) and `2026-07-06-collaboration-projects-design.md` (Stage II), closing D-1 (iroh) and D-2 (email OTP) and adding D-8…D-11 below. Phase 2 calibration has since **shipped** (v0.3.0), so C-6 is no longer blocked. The build order in §5 is superseded by the **Perseus-first** two-track sequencing in the Stage I design (S-1/S-2 spikes collapsed into it); §4/§5 kept for history.

## 1. System overview

```
┌───────────────── account of user 1 ─────────────────┐
│  home app (primary)   Perseus agent (capture, C-10)  │   other users' apps
│                       or full app as capture      ×N │
│      ▲   │ P2P transfer (C-4/C-5)  │   ▲             │      ▲            ▲
│      └───┴─────────────────────────┘   │      │ P2P per project roles (C-7)
└───────┼────────────────────────────────┼──────┘      │            │
        │ HTTPS (auth, devices,          │             │            │
        │ relay map, projects, stats)    │             │            │
        ▼                                ▼             ▼            ▼
   ┌─────────────── C-1 hub (central service) ───────────────┐   ┌──────────┐
   │ accounts · devices · relay maps · projects · roles ·    │   │ C-8      │
   │ thresholds · contribution accounting · stats            │   │ relays   │
   └──────────────┬───────────────────────────┬──────────────┘   │ (public +│
                  ▼                           ▼                  │ private$)│
        ┌── C-2 portal (web) ──┐    ┌── C-9 Discord bot ──┐      └──────────┘
        │ directory · project  │    │ private channel per │
        │ pages · profiles     │    │ project (Phase III) │
        └──────────────────────┘    └─────────────────────┘
```

Invariants (from BRD): hub never carries image payloads; catalogs stay authoritative for metadata; app fully functional with no account.

## 2. Components

### C-1 Hub — central service (new codebase)

Accounts + auth (D-2), device registry per account (list/revoke), relay-map distribution (per account; per project for paid private relays — BRD Q2), project registry (publish, join requests, membership, roles, coordinator handover, thresholds), contribution accounting (aggregates only: counts/integration/filters, reported by apps), imaging/transfer statistics. Plain HTTPS JSON API; the desktop app and portal are its two clients.

- **D-1 stack:** Axum + Postgres is the default (team already runs Axum in athenaeum-web); single deployable.
- **D-4 repo:** separate repo (`athenaeum-hub`), not the app workspace — different release cadence, different deploy target. Portal ships from the same repo.
- Explicitly stateless with respect to image data and thin on metadata: it stores project/membership/threshold state and aggregates, never frame-level metadata (frame lists live in the participating catalogs; accounting reports are summaries).

### C-2 Portal — web frontend on hub API

Project directory, project page (description, members, progress, join button, chat link), sign-in (same account), profiles (Phase III). React — reuses app frontend conventions but is a separate build (no Tauri/SSE machinery).

### C-3 App account layer

Sign-in from desktop app, device identity (keypair generated on first sign-in; the public key doubles as the transport node ID — one identity for hub and P2P), token storage in OS keychain, `Settings → Account` UI. Additive: absence of account disables only sync/collab surfaces (BRD A2).

### C-4 Transport adapter

The `SharingTransport` trait from the pillar-C sketch, hardened to the BRD semantics. Surface (conceptual): `send(file, to_device) → TransferHandle`, delivery confirmation callback (content-hash-verified), resume-after-restart, progress events into the existing `ProgressEmitter`, connectivity via hub-issued relay map. Two candidate impls — **iroh embedded** vs **Syncthing sidecar** — resolved by spike S-1 (BRD Q9; monetization/licensing weight favors iroh, Q2). Everything above this trait is transport-agnostic by construction.

### C-5 Sync engine (app-side; the largest app component)

Owns BRD section B end-to-end:

- Machine role config (primary / capture node, linked to account devices).
- Outbound queue on the capture node: auto mode (fed by the existing scanner/monitor "new file" path) and manual mode (fed from selection UI). Runs on the existing `operation_queue` worker.
- Delivery pipeline per file: transfer (C-4) → receiver verifies + ingests → receiver acks (frame uuid + content hash) → sender marks *transferred* → retention becomes applicable. Reuses the **package/manifest format** (roadmap Phase 4: uuid-keyed manifest + payload) as the unit of transfer so metadata travels with files (B8).
- Retention service on capture nodes: policy evaluation (immediate / N days / disk ≥ X% oldest-transferred-first), hard rule "never delete untransferred" (B4/B5).
- Transfer history: local DB tables on both ends (append-only, searchable), fed to hub as aggregate stats only.
- Ingest on primary: dedupe by frame uuid/content hash (needs roadmap Phase 1 UUIDs), metadata merge with primary-wins rule (B9).

### C-6 Quality gate (app-side)

Per-project threshold config (from hub) evaluated locally against analysis results + calibration status → per-frame publishable flag + failure reasons (BRD C4). Mostly queries over existing `frames`/analysis tables + the Phase-2 calibration state. **Direct dependency on the in-flight Phase 2 work** — the gate consumes "frame is calibrated" and analysis metrics; version-stamp the pipeline per Q4.

### C-7 Project client (app-side)

Local representation of joined projects: membership + roles fetched from hub, mapping roles onto transport topology (who this node sends to / receives from), publish action (moves gate-passing frames into the project exchange), receive-side ingest into a per-project staging area of the catalog with `origin_catalog_uuid` provenance. Contribution accounting reports to hub.

### C-8 Relay infrastructure (ops)

Public relays (default) + private per-project relays (paid tier). Provisioning + relay-map gating via hub. Stateless, cheap horizontal scaling; concrete binary depends on D-1/S-1 outcome (iroh-relay fork with token auth, or strelaysrv).

### C-10 Perseus — lightweight capture-node agent (BRD B1a)

A separate small binary for observatory machines: sign in with the account, pick the target Athenaeum installation, point at the capture directory — done. Watches the directory, parses FITS/XISF headers (reusing `fits_parser`) to build manifests, packages (W7), sends (C-4), applies retention (C-5 rules), keeps local transfer history. **No catalog DB, no library UI**; configuration via a minimal local web page (Syncthing-style) or config file; runs as a service/daemon; targets modest hardware (incl. Linux ARM mini-PCs). Automatic mode only — manual triage stays a full-app capture-node feature.

**Structural requirement this imposes:** the sync engine (C-5) must be built as a reusable library layer in `athenaeum-core` (queue, packaging, delivery/confirmation, retention, history) with two shells: the full app and Perseus. Perseus = account client (C-3 subset) + directory watcher + that library + minimal config UI.

### C-9 Discord bot (Phase III)

Hub-driven: create private channel per project, invite members by Discord nick, post milestone events. v1 of chat is just a link field on the project (BRD C7) — no bot.

## 3. BRD-requirement → component map

| BRD | Component(s) |
| ---- | ---- |
| A1–A3 | C-1, C-3 (+C-8 for relay lists) |
| A4–A5 | C-1, C-2 |
| B1–B10 | C-5 over C-4 (C-3 for identity) |
| C1–C3a | C-1 (state) + C-7 (enforcement) + C-2 (UI) |
| C4 | C-6 |
| C5 | C-7 over C-4 |
| C6, C9 | C-7 reporting → C-1 → C-2 |
| C7 | C-2 (link field) → C-9 (bot, later) |
| D1–D4 | C-2 |

## 4. Spikes (close blocking decisions)

| # | Question | Shape | Exit artifact |
| ---- | ---- | ---- | ---- |
| S-1 | Transport (Q9/D-1a): run the B4/B5 scenario on both candidates — two machines, ~10 GB of FITS, kill network mid-transfer, resume, hash-confirmed delivery, delete-at-source; plus relay self-host test | timebox ~1 wk total, standalone playground repo | decision record + measured notes; trait surface validated |
| S-2 | Auth flow (Q1/D-2): account provider choice + desktop sign-in flow (device-code style vs embedded webview), keychain storage | 2–3 days | decision record + flow diagram |
| S-3 | Hub skeleton: accounts/devices/relay-map endpoints only, deployed once end-to-end with the app account layer stub | ~1 wk | walking skeleton |

## 5. Build order & BRD phase mapping

```
now ──► app foundation (roadmap P1: uuids, ts-rs)      [independent of Phase 2 calibration]
    ──► S-1, S-2 spikes                                 [independent, playground]
    ──► S-3 hub skeleton (C-1 minimal, C-3)
         └─► C-4 chosen impl → C-5 personal sync  ═══ BRD Phase I ships
Phase 2 (calibration) completes ─► C-6 quality gate
         └─► C-1 projects + C-7 + C-2 portal      ═══ BRD Phase II ships
              └─► C-9 bot, A4/A5 stats+profiles   ═══ BRD Phase III
              └─► C-10 tile-plan projects (mosaic pillar), private-relay tier ═ Phase IV
```

**Can start now without touching the calibration work:** roadmap-P1 foundation, S-1/S-2 spikes, S-3 hub skeleton, C-4 trait definition. **Must wait for Phase 2:** C-6 (consumes calibration state), and therefore Phase II's publish flow. Personal sync (BRD Phase I) has **no dependency on calibration** — it can ship while Phase 2 finishes.

## 6. Decision log

| # | Decision | Status |
| ---- | ---- | ---- |
| D-1 | Transport impl = **iroh embedded**; Syncthing stays the fallback behind the C-4 trait; validation folded into the Perseus-first track | **closed** (owner, 2026-07-06 — personal-sync design §2) |
| D-2 | Auth = **passwordless email OTP**; opaque hashed per-device tokens; optional Discord OAuth link with collab features | **closed** (owner, 2026-07-06 — personal-sync design §2) |
| D-3 | Hub stack: Axum + Postgres, single deployable | accepted (2026-07-06) |
| D-4 | Hub+portal in a separate repo (`athenaeum-hub`); portal SPA served by the same Axum binary (one origin, `projects.artfrom.space`) | accepted (2026-07-06) |
| D-5 | Device keypair doubles as transport node identity | accepted (2026-07-06) |
| D-6 | Package/manifest is the transfer unit for both personal sync and projects; `payload_kind` future-proofs raw+masters mode | accepted (2026-07-06) |
| D-7 | Sync engine as a shared `athenaeum-core` library with two shells: full app + Perseus agent (C-10); Perseus is auto-mode-only; core feature-gated so Perseus builds without rustafits | accepted (owner, 2026-07-05; feature-gating added 2026-07-06) |
| D-8 | Collab delivery = **hub as tracker + members as swarm**: package-level announcements + have-reports on hub, any receive-role holder serves; store-and-forward deferred to Phase IV paid tier. **Exchange refined 2026-07-12** (Phase-1/2/3 primitives): per-holder **Offer/Want** fetch, project-scoped **`PeerAuthorizer`**, active **push-seed**, mirror-landing + reconcile-adopt; parallel multi-source fetch deferred. | accepted (owner, 2026-07-06 — collab design §2; refined 2026-07-12 — collab design §2/§2a + review) |
| D-9 | **Portal owns administration, app owns data**: publish form/approvals/roles/thresholds/handover on portal; gate/publish/receive/replication in the app; they meet at deep links | accepted (owner, 2026-07-06 — collab design §6) |
| D-10 | **Perseus-first sequencing**: Perseus MVP (ticket pairing behind a dev flag, dry-run retention) is the iroh validation vehicle on the owner's real observatory; hub decoupled from the transport critical path | accepted (owner, 2026-07-06 — personal-sync design §3/§7) |
| D-11 | **Received contributions stay outside the frames catalog**: `ATH_PRJ` provenance card + `project_contributions` table + scanner branch; UI = project Receive tab; project-scoped WBPP export for processors | accepted (owner, 2026-07-06 — collab design §7) |
