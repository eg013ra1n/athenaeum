# Collaboration Projects (Stage II) — Design — 2026-07-06

**Status:** Approved in brainstorm 2026-07-06, pending owner review of the written spec. **Owner:** Vilen.
**Inputs:** BRD `2026-07-05-sync-collaboration-brd.md` (sections C, D + A5), components doc `2026-07-05-sync-collaboration-components.md` (C-1, C-2, C-6, C-7), Stage I design `2026-07-06-personal-sync-design.md` (transport, package format, engine — all reused). **Dependencies:** Stage I shipped (transport + engine + hub + accounts); Phase 2 calibration + B5 light calibration shipped (v0.3.0) — the quality gate consumes both. Closes BRD Q4 (trust+stamp) and Q5 (calibrated-only v1); amends one BRD non-goal (§2).

---

## 1. Scope

BRD Phase II: publish a project, join requests + roles, quality gate, P2P exchange between members, contribution accounting, portal directory + project pages (C1–C6, D1–D2). §10 sketches Phase III (Discord bot, verified profiles, stats) as outlook, not design. Out of scope: tile-plan projects (C10, needs the mosaic pillar — deferred), completion states (C11), moderation tooling (Q6 — required before *public* directory launch, tracked as an open item).

## 2. Delivery semantics: hub as tracker, members as swarm

The hard product problem: P2P with no cloud storage means a transfer needs sender and receiver online simultaneously — and hobby desktops are off during the day. Design:

- **Key observation:** a processor (send+receive role) receives *everything* by definition — so every processor is a full replica. With content-addressed blobs, **any member holding a blob can serve it**. A contributor only needs to overlap with *one* always-on member (processors are exactly the NAS-owning kind); data then spreads from replicas. Torrent semantics, nearly free on iroh-blobs.
- **Hub = tracker**: publishing announces `(project_id, package_id, root hash, byte size, frame count, aggregate quality stats §5)` to the hub. Members report back "I hold package X" (**have-reports**). Receivers see what exists that they lack — even while the original publisher is offline — and fetch from any online holder. The hub carries hashes and counters, never payloads and never per-frame metadata.
- **BRD amendment (explicit):** the non-goal "central service never stores image data" is refined to "never stores or proxies image payloads **and never stores frame-level metadata**; it may store package-level announcements (ids, content hashes, sizes, counts, aggregate quality stats)". Tracker role, not storage role.
- **Role enforcement:** the hub signs membership snapshots (member node ids + data roles, versioned). A provider serves a blob only to node ids with receive role in a current snapshot. Leaving/removal (C8) = next snapshot excludes the node; serving and fetching stop (already-delivered data handling per the project's data policy, Q3).
- **Propagation is poll-based in v1:** apps refresh membership/thresholds/announcements on start, periodically (minutes), and on manual refresh. Role changes take effect "for subsequent exchange" (BRD C3) — poll cadence satisfies that. SSE push from hub is a later optimization.
- **Store-and-forward** (encrypted TTL spool on *private paid relays*) fully solves asynchrony and is the natural premium feature — **Phase IV candidate**, requires a second, bigger BRD amendment; explicitly not v1.
- **Honest UX:** contributor sees "delivered to k of m receivers — keep this machine online until at least one processor holds it", then a notification "your contribution is replicated — safe to go offline". Project page shows per-package holders and how many sources are online.

## 3. Publication unit

**Calibrated-only in v1** (closes Q5): the exchange payload is the B5 calibrated float FITS (`CALSTAT`, `ATH_C*` provenance cards already in the headers). Rationale: the product's differentiator is a *uniform, ready-to-register* dataset — WBPP with calibration off, zero per-member calibration matching on the receiving side. Raw+masters mode would halve bytes (float32 = 2× int16 — honest numbers: IMX571 ~104 MB/frame calibrated; a 5×200-sub project ≈ 100 GB to each processor) but double receiver-side complexity; the swarm and resumable transfers absorb the size, nothing absorbs the complexity. Calibration-quality variance between members exists in *both* modes; calibrated-only at least stamps it with provenance.

- The manifest's `payload_kind` (Stage I §5) keeps `raw_frame | master` values reserved so a per-project raw+masters mode is a Phase IV extension, not a format break.
- **Granularity:** blob = one calibrated frame; publication = batch collection (hash-seq) + manifest. Dedupe and per-frame resume come from content addressing.
- **Supersede:** frame identity = source frame `uuid` (`ATH_CSRC`). Re-publication of the same uuid with new content (re-calibration, newer `ATH_CVER`) replaces the older version in project state — resolved by receivers from manifests; the hub still sees only package-level announcements.
- Per-frame manifest metadata: source-frame snapshot (exposure/filter/date-obs/optics), analysis metrics, calibration provenance, version stamps (§5). Raw sources never leave the contributor.

## 4. Quality gate (C-6)

Two layers, evaluated **locally** in the contributor's app; status is **derived, never stored** (same philosophy as `light_calibrations::derive_status` — no staleness bugs; only the fact of publication persists).

**Layer 1 — hard preconditions (always on, not configurable):**

1. Calibrated: `derive_status = calibrated` (not stale/partial).
2. Analysis present (`frame_analysis` row exists for the frame).
3. Pixel scale known: plate-solve scale, else XPIXSZ/FOCALLEN header derivation (mind the known XPIXSZ-already-binned gotcha). Unknown → not publishable, reason "unknown pixel scale".
4. On-target: frame center (solved WCS, else OBJCTRA/DEC) within the project's target radius. Not in the BRD; added as cheap insurance against "linked the wrong set" — and it keeps working even if a wrong set is linked manually (§7).

**Layer 2 — coordinator thresholds:** list of rules `{metric_key, op, value}`, per-project, versioned, stored on hub, edited on the portal. v1 metric registry (canonical units — cross-setup comparability is the point):

| metric | unit / source | default rule |
| ---- | ---- | ---- |
| `fwhm_arcsec` | `median_fwhm` px × pixel scale | ≤ (primary threshold) |
| `eccentricity` | `median_eccentricity` | ≤ |
| `stars_detected` | count | ≥ (anti-cloud) |
| `not_trailed` | `possibly_trailed` = 0 | reject if trailed (on by default, coordinator can disable) |

SNR family (`median_snr`, `snr_weight`, `frame_snr`) is **in the registry but flagged "advanced"**: exposure- and gear-dependent, no honest cross-setup comparison; the portal threshold editor shows a comparability warning. The registry is extensible — adding a metric later costs nothing.

**Q4 = trust + stamp:** the gate runs on the contributor's machine; the manifest carries, per frame: metric values, analysis engine version + `config_hash` (already stored on `frame_analysis`), calibration `ATH_CVER`, and the threshold-set version it passed. Coordinator dashboards can surface mixed-version contributions; receiver-side re-validation is possible later precisely because values+stamps travel — not built in v1.

**Threshold changes are prospective:** published stays published (immutable history); new rules gate future publications.

**Coordinator visibility without frame-level data on hub:** package announcements include aggregate quality stats (median/p10/p90 `fwhm_arcsec`, median eccentricity, counts, integration by filter) → C6 accounting and C9 dashboards are built from aggregates. Frame-level detail is visible to receive-role members from manifests.

**Contributor UX:** project view lists candidate frames with per-rule pass/fail and reasons ("FWHM 3.4″ > 3.0″", "not calibrated", "no analysis", "outside target radius"); the publish action takes passing frames only.

## 5. Hub: project model + API (C-1 extension)

Tables (Postgres, sketch): `projects` (target name/RA/Dec/radius, title, description, goals, chat_link, data_policy_text, status), `project_members` (account, node ids, data_role send|send_receive, joined_at; coordinator flag — exactly one, transferable per C3a with history), `join_requests` (desired_role, message, profile snapshot, decided_by/at), `project_thresholds` (versioned rule sets), `package_announcements` (package id, root hash, size, counts, aggregate stats, publisher, superseded flags), `have_reports` (member × package), `project_events` (handover, membership changes — audit trail).

API: CRUD per the flows in §8; membership snapshot endpoint (signed); announcements + have-report ingest; aggregates for portal pages. All under the same `projects.artfrom.space/api/v1` origin.

## 6. Portal (C-2)

**React SPA (Vite + Tailwind, app conventions; separate build, no Tauri/SSE) living in the `athenaeum-hub` repo, served by the same Axum binary as the API** — one origin, no CORS, one deploy.

- **Discord/social embeds:** Axum injects OG meta tags into the HTML shell for `/p/:slug` (and later profile) routes — a project link pasted in Discord unfurls with target/progress. Full SEO prerendering deferred until organic search matters.
- **Pages v1:** directory (browse/search public projects), project page (description, target, members, per-member contribution + totals from aggregates, holders/online-sources per package, join button, chat link), new-project form (§8, supports URL-prefill), coordinator admin (join-request queue, roles, threshold editor with comparability warnings, handover, data-policy template), sign-in (email OTP, session cookie).
- **Admin lives on the portal, data lives in the app** (decision): approve/reject, roles, thresholds, handover, project publishing — portal (works from a phone); gate evaluation, publishing frames, receiving, replication status — app. The two meet only at deep links.
- Domain: `projects.artfrom.space` on the existing VPS behind nginx, GitLab CI deploy. Relay stays on its own hostname (Stage I).

## 7. App-side project client (C-7)

**New sidebar page "Projects".** Cards: target, my role badge, gate summary ("12 publishable of 47"), replication state of my publications, pending-incoming count. Detail view, three tabs:

- **Contribute** — "Linked objects" section (below) + candidate frames of linked sets with per-rule gate results + "Publish N passing frames"; publication history with per-package "delivered to k of m, held by …".
- **Receive** (receive role only) — announced packages: available (sources online/offline) / downloading (progress, speed) / done (path, verified); per-project storage destination.
- **Overview** — members, roles, thresholds (read-only), totals, "Manage on portal" link.

**Project ↔ object linking (explicit, not auto):** `project_links (project_id, frames_set_id)` — **local table, never sent to hub**. A picker ranks candidate frame sets by great-circle distance of set center to the project target (within radius first) with OBJECT-name similarity as a secondary signal; manual pick of any set allowed (warned if off-target — layer-1 precondition 4 still guards per frame). Multiple links per project (seasons/rigs); gate evaluates the union. Empty state: "Link an object to start". **Join-first-shoot-later:** when scanning/clustering creates or grows a set whose center falls within a linked project's target radius → discrete notification "Frame set matches project X — link it?"; never auto-link (user stays in control). Coordinator's source set auto-links on create-from-app. Unlinking drops candidates; published history is untouched.

**Received contributions — catalog boundary (important):** received calibrated frames carry `CALSTAT` + `ATH_CSRC`, and the scanner's reconcile-adopt would look for the source frame in the local catalog — which a processor doesn't have → eternal deferred-adopt warn. Stage II extends the scanner: a file with calibration cards **plus a project provenance card (`ATH_PRJ` = project id, written at publish time)** is a *received contribution* — tracked in a new `project_contributions` table (frame metadata from the manifest), **never enters `frames`/clustering/sessions/duplicates** — same philosophy as own B5 artifacts. Disk layout under a managed root: `<CollabRoot>/<project-slug>/<member>/…` (Calibration Library pattern). Their UI is the Receive tab, not Objects. A project-scoped WBPP export entry point for processors ("export combined dataset, calibration off") ships within Stage II — it is the processor's payoff.

**Transfers:** project exchange rides the same engine tables + Transfers surface from Stage I (a `project` dimension on queue/history rows); TransferIndicator aggregates both. Every serve to a peer is an outbound history row (what, to whom, when, bytes) — G3 extends to collaboration.

## 8. End-to-end flows

**Publish a project (coordinator):**

1. Entry points: in-app "Publish as project" on a frame set / Projects page → opens `projects.artfrom.space/new?object=…&ra=…&dec=…` prefilled from the catalog; or the portal form directly (manual target entry; SIMBAD name resolution = later nicety).
2. Form: target (name, RA/Dec, radius), title/description, integration goals (hours per filter, optional — feeds C9), initial thresholds (template defaults: FWHM ≤ 3.5″, ecc ≤ 0.6, trailed reject), **coordinator's own data role**, chat link (optional v1 field), data-policy template text (Q3).
3. Submit → live in the public directory → appears in the coordinator's app on next membership poll (coordinator badge); source set auto-linked. From here the coordinator contributes like any member.

**Join (participant):**

1. Discovery on the portal only (directory, shared links with Discord embeds); the app's "Browse projects" is a portal link.
2. "Request to join" (sign-in required; no account → email-OTP signup inline). Request carries **desired role** (contribute/process/both — coordinator still decides), short message, profile card.
3. Coordinator gets email + portal badge (the "70% decided in 7 days" metric lives on email nudges); approves with role assignment or rejects — on the portal.
4. Participant gets email; project appears in their app on next poll. First-open: receive role → storage-destination prompt (creates `<CollabRoot>/<project-slug>/`); everyone → link-object prompt, gate starts evaluating, transport picks up the membership snapshot.

**Leave/removal:** portal action → next snapshot excludes the node → serving/fetching stop (C8). Invite-by-link: Phase III backlog (BRD only requires join requests).

## 9. Notifications

Discrete outcomes only, `notify()` conventions; new kind `project` (+ Stage I's `sync`): "your contribution is replicated", "new package available in <project>", "download complete", "frame set matches project X — link it?", "join request approved". Dedupe keys = package/project ids.

## 10. Stage III outlook (sketch, not design)

Discord bot (C-9): hub-driven private channel per project, invites via the OAuth-verified Discord link from D-2, milestone events posted. Profiles (A5): public pages on the portal, Discord nick shown verified. Stats (A4): imaging/transfer dashboards from aggregate reports; telemetry consent per Q8. Design these against real Stage II usage.

## 11. Testing

Gate: unit tests over fixture `frame_analysis` rows (each precondition + each rule + unit conversion + the binning gotcha). Delivery: multi-node swarm tests over `LoopbackTransport` (publisher offline after first replica; role enforcement; supersede; have-report convergence); membership-snapshot revocation test. Scanner: received-contribution branch (with/without `ATH_PRJ`, dedupe on re-scan). Hub: API integration tests; snapshot signature verification. E2E (extends the Stage I harness): three instances — contributor, processor, coordinator — publish → join → gated publish → swarm delivery → combined WBPP export.

## 12. Open items (carried, with owners)

- Q3 data-policy template text (owner + community) — needed for the publish form.
- Q6 minimal moderation (report button + hub-side hide + email contact) — before the directory is publicly announced.
- Q7 coordinator disappearance escalation — not blocking; revisit after real projects exist.
- Store-and-forward premium tier (Phase IV) — pricing/economics with Q2.
