# Collaboration Projects (Stage II) — Design — 2026-07-06

**Status:** Approved in brainstorm 2026-07-06. **Owner:** Vilen. **Exchange layer revised 2026-07-12** — see the callout in §2 and the review `2026-07-12-collaboration-exchange-review.md`. **Round 2 (owner, 2026-07-12, mockup walkthrough):** per-project contribution-**approval** toggle with in-app moderation (§2 approval bullet, §7 Moderation tab), explicit **permissions matrix** — coordinator-only in v1 (§5a), portal/app UI validated against mockups, **build order fixed** — hub-first in five slices, app side on branch `0.5.0` (§13).
**Inputs:** BRD `2026-07-05-sync-collaboration-brd.md` (sections C, D + A5), components doc `2026-07-05-sync-collaboration-components.md` (C-1, C-2, C-6, C-7), Stage I design `2026-07-06-personal-sync-design.md` (transport, package format, engine — all reused). **Now also builds on the P2P primitives shipped after this doc:** the mesh model `2026-07-11-sync-model-phase1-design.md` (capability, account-wide `PeerAuthorizer`, explicit-target send + per-peer engines, mirror landing), the dedup handshake `2026-07-12-sync-dedup-handshake-design.md` (Offer/Want), and the self-describing-file reconcile-adopt (Plan 2C light-cal). The 2026-07-12 revision folds those in — the collab exchange is now assembled from shipped, reviewed primitives, not a bespoke transport. **Dependencies:** Stage I shipped (transport + engine + hub + accounts); Phase 2 calibration + B5 light calibration shipped (v0.3.0) — the quality gate consumes both. Closes BRD Q4 (trust+stamp) and Q5 (calibrated-only v1); amends one BRD non-goal (§2).

---

## 1. Scope

BRD Phase II: publish a project, join requests + roles, quality gate, P2P exchange between members, contribution accounting, portal directory + project pages (C1–C6, D1–D2). §10 sketches Phase III (Discord bot, verified profiles, stats) as outlook, not design. Out of scope: tile-plan projects (C10, needs the mosaic pillar — deferred), completion states (C11), moderation tooling (Q6 — required before *public* directory launch, tracked as an open item).

## 2. Delivery semantics: hub as tracker, members as swarm

The hard product problem: P2P with no cloud storage means a transfer needs sender and receiver online simultaneously — and hobby desktops are off during the day. Design:

> **2026-07-12 exchange revision.** This section originally hand-rolled its exchange over the Stage-I engine. The primitives shipped since (mesh model + dedup handshake, Phase 1–3) now supply most of it directly, so the delivery model is re-expressed in terms of them below: the **swarm fetch** is a per-holder **Offer/Want** negotiation (Plan 3), **role enforcement** is a project-scoped **`PeerAuthorizer`** (Plan 2A), the contributor **actively push-seeds** a first replica (explicit-target send, Plan 2C), and landing/ingest reuse the mirror-landing + reconcile-adopt paths (§7). Parallel multi-**source** fetch is the one genuinely-new engine capability the swarm wants and is deferred (last bullet). Full mapping + a cross-account threat model: the review doc.

- **Key observation:** a processor (send+receive role) receives *everything* by definition — so every processor is a full replica. With content-addressed blobs, **any member holding a blob can serve it**. A contributor only needs to overlap with *one* always-on member (processors are exactly the NAS-owning kind); data then spreads from replicas. Torrent semantics, nearly free on iroh-blobs.
- **Hub = tracker**: publishing announces `(project_id, package_id, root hash, byte size, frame count, aggregate quality stats §5)` to the hub. Members report back "I hold package X" (**have-reports**). Receivers see what exists that they lack — even while the original publisher is offline — and fetch from any online holder.
  - **The fetch from a holder is a per-holder Offer/Want negotiation (Plan 3):** the receiver pulls **only the frames it is missing** from that holder (sampling-hash → full-hash confirm), so a partially-replicated receiver **resumes across holders** with no duplication (BRD B7 / C5 uuid+content dedup, now a negotiated fact rather than post-fetch cleanup), and a re-published/**superseded** frame (same `ATH_CSRC` uuid, new content/`ATH_CVER`, §3) has a new content hash → lands in the `want` set → transfers → the receiver resolves uuid-supersede from the manifest. Best-effort: a holder that doesn't speak the handshake → full-package fetch fallback.
  - The hub carries hashes and counters, never payloads and never per-frame metadata.
- **BRD amendment (explicit):** the non-goal "central service never stores image data" is refined to "never stores or proxies image payloads **and never stores frame-level metadata**; it may store package-level announcements (ids, content hashes, sizes, counts, aggregate quality stats)". Tracker role, not storage role.
- **Role enforcement = a project-scoped `PeerAuthorizer` (reuses Plan 2A):** the hub signs membership snapshots (member node ids + data roles, versioned). Instead of the ad-hoc "check the snapshot on each serve," reuse the mesh receiver's fail-closed cached allow-list primitive, fed by a **project-membership source** (the receive-role node ids in each project's current signed snapshot) rather than the account device list. Fail-closed default (empty cache authorizes nobody until the first snapshot refresh) is exactly right cross-account. Leaving/removal (C8) = next snapshot excludes the node → the authorizer drops it → serving and fetching stop (already-delivered data handling per the project's data policy, Q3). **Data role ≈ a project-scoped capability:** `send-only` behaves like the mesh `perseus` capability (never a serve destination — reuse the "send-only is never a destination" enforcement); `send+receive` like a full peer. Scope is project-membership (coordinator-assigned, in the snapshot), NOT the account-global capability — don't overload the latter.
- **Active push-seed the first replica (reuses Plan 2C explicit-target send):** the async pain — "keep this machine online until a processor holds it" — is attacked directly. On publish, when an online receive-role holder exists in the snapshot, the contributor **pushes** the package straight to it (per-peer engine, explicit destination) and can go offline as soon as that one replica confirms; the swarm spreads from there. Announce-and-wait is the fallback when no receive-role holder is online. UX: "seeding to `<processor>`…" → "replicated — safe to go offline", far sooner than a poll-triggered pull.
- **Contribution approval — per-project toggle (owner, 2026-07-12):** `projects.require_approval`, set by the coordinator at creation (editable in settings). **OFF:** publish → announcement is born `published`; thresholds are the only gate (original design). **ON:** the announcement is born **`pending`**; the publish push-seed targets the **coordinator** (the create form enforces that a manual-approval project's coordinator holds `send+receive` — you can't review what you can't receive); only the coordinator may fetch a pending package; members never see it (holders don't serve pending, receivers don't list it). The coordinator reviews **in the app, on the data** (real frames + per-frame metrics — Moderation tab, §7) and decides on the hub: **approve** → `published`, normal swarm serving begins; **reject** → `rejected` with a reason, contributor notified, never served, the coordinator's local review copy is removed. The portal shows only the pending count + aggregates (read-only) — the decision needs the data, so it lives in the app (D-9 exception, deliberate). Two edges: a coordinator **offline at publish time** → the pending package announce-and-waits *toward the coordinator only* (contributor UX: "waiting for <coordinator>" instead of "safe to go offline"); **handover with pending packages** → they stay `pending`, the new coordinator fetches them the same way (the contributor — and the former coordinator, who keeps its review copy as a holder — serve it).
- **Propagation is poll-based in v1:** apps refresh membership/thresholds/announcements on start, periodically (minutes), and on manual refresh. Role changes take effect "for subsequent exchange" (BRD C3) — poll cadence satisfies that. SSE push from hub is a later optimization.
- **Store-and-forward** (encrypted TTL spool on *private paid relays*) fully solves asynchrony and is the natural premium feature — **Phase IV candidate**, requires a second, bigger BRD amendment; explicitly not v1.
- **Parallel multi-source fetch = the one genuinely-new engine capability the swarm wants, DEFERRED.** "Any holder can serve; data spreads from replicas" ideally means a receiver assembling one package from *several holders in parallel* (BitTorrent piece selection). Offer/Want gives the per-holder content-diff and content-addressing makes any blob fetchable from any holder, but the engine today fetches a whole collection by `root_hash` from **one** provider. **v1 = sequential per-holder Offer/Want** (buys the dedup/resume wins immediately, no engine rewrite). **True parallel multi-source** — split the want-set across all online holders, fetch blobs concurrently by hash — is a defined Stage-II performance follow-up; iroh-blobs supports per-blob fetch from any provider, so it is a fetch-**scheduling** extension over the same content-addressed blobs, **not a protocol or format break**.
- **Honest UX:** with a push-seed the contributor sees "seeding to `<processor>`…" → "your contribution is replicated — safe to go offline"; the announce-and-wait fallback shows "delivered to k of m receivers — keep this machine online until at least one processor holds it". Project page shows per-package holders and how many sources are online.

## 2a. Cross-account trust boundary (security model)

The mesh model's posture — "**every device in my account is mine and mutually trusted**" — does **not** transfer: a project mixes different users' nodes. So the reused primitives keep their mechanism but re-anchor trust, and the receiver-side validation the mesh work built becomes *essential* rather than prudent. (The mesh Phase-1 security note explicitly deferred the cross-account case "to the separate Stage II collaboration design" — this is it.)

- **Trust anchor = the hub-signed membership snapshot, not account membership.** A node is served/served-to only if it is in a *current, signature-verified* snapshot; leaving/removal (C8) excludes it from the next snapshot. Snapshot signature verification is the trust root (already in §11 tests).
- **Receiver-resolves everything; never trust the package.** Plan 2B's "the landing member-slug is resolved by the receiver, never from the package" and Plan 3/2C's "`rel_path` is `validate_rel_path`-guarded on both write and receive" move from prudent to load-bearing. A malicious member's manifest must never steer a path, a directory, or a dedup skip.
- **On-wire content-hash verification is the defense against a malicious holder** serving wrong bytes for a hash (C-4 already verifies fetched content against the hash; keep it — a swarm holder is not the publisher).
- **A malicious `Want`/receiver cannot cause data loss at the sender.** Raw sources never leave the contributor (§3), and a receiver answering "I want nothing" only skips a transfer it claims to already have — it can't delete the sender's data. (The mesh "empty-Want → Confirmed → delete-after-confirm retention" note is a *personal-sync* retention concern; collab publications are the contributor's own retained data.)
- **Quality-gate stamps travel from v1 for later receiver-side re-validation (§4, Q4).** Cross-account, re-checking a contributor's claimed metrics (values + engine version + `config_hash` + threshold-set version, in the manifest) matters more than same-account. Not built in v1, but the stamps must travel — they already do.

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

`projects` additionally carries `require_approval` (§2); `package_announcements` carry `state pending|published|rejected` + `decided_by/decided_at` + `reject_reason` — the state machine is `pending → published|rejected` (or born `published` when the toggle is off), never backwards.

API: CRUD per the flows in §8; membership snapshot endpoint (signed); announcements + have-report ingest; approve/reject endpoints (coordinator-authenticated, §2); aggregates for portal pages. All under the same `projects.artfrom.space/api/v1` origin.

## 5a. Permissions (v1: coordinator-only)

One admin role — the **coordinator** (exactly one per project, transferable with history, C3a). Delegated roles (e.g. a member who can moderate contributions or join requests) are a **Phase III backlog item** — design against real usage. The matrix below is normative; the underlying principle is D-9 — admin actions on the portal (phone-friendly, propagate via the signed snapshot on the poll cadence), data actions in the app. The single deliberate exception is contribution approval: admin in spirit, but it needs the frames, so it lives in the app (§2).

| Action | Who | Where |
| ---- | ---- | ---- |
| Create a project | any signed-in account | portal (`/new`, app deep-links with catalog prefill) |
| Edit description / goals / chat link / data policy | coordinator | portal |
| Join requests: approve (with role) / reject | coordinator | portal |
| Change a member's data role / remove a member | coordinator | portal |
| Coordinator handover | coordinator | portal |
| Quality thresholds (versioned) | coordinator | portal |
| `require_approval` toggle | coordinator | portal (create form + settings) |
| Approve / reject contributions | coordinator | **app** (on the data, §2/§7) |
| Publish frames (through the gate) | member with `send` or `send+receive` | app |
| Receive project data | member with `send+receive` | app |
| Leave the project | any member | portal |

## 6. Portal (C-2)

**React SPA (Vite + Tailwind, app conventions; separate build, no Tauri/SSE) living in the `athenaeum-hub` repo, served by the same Axum binary as the API** — one origin, no CORS, one deploy.

- **Discord/social embeds:** Axum injects OG meta tags into the HTML shell for `/p/:slug` (and later profile) routes — a project link pasted in Discord unfurls with target/progress. Full SEO prerendering deferred until organic search matters.
- **Pages v1:** directory (browse/search public projects), project page (description, target, members, per-member contribution + totals from aggregates, holders/online-sources per package, join button, chat link), new-project form (§8, supports URL-prefill; includes the `require_approval` toggle and enforces coordinator `send+receive` when it is on), coordinator admin (join-request queue, roles, threshold editor with comparability warnings, handover, data-policy template, approval toggle + **pending-contributions counter with "review in app"** — read-only, §2), sign-in (email OTP, session cookie). Validated against mockups 2026-07-12 (owner).
- **Admin lives on the portal, data lives in the app** (decision): approve/reject, roles, thresholds, handover, project publishing — portal (works from a phone); gate evaluation, publishing frames, receiving, replication status — app. The two meet only at deep links.
- Domain: `projects.artfrom.space` on the existing VPS behind nginx, GitLab CI deploy. Relay stays on its own hostname (Stage I).

## 7. App-side project client (C-7)

**New sidebar page "Projects".** Cards: target, my role badge, gate summary ("12 publishable of 47"), replication state of my publications, pending-incoming count. Detail view, three tabs (+ a coordinator-only fourth, layout validated against mockups 2026-07-12):

- **Contribute** — "Linked objects" section (below) + candidate frames of linked sets with per-rule gate results + "Publish N passing frames". **Publish = build the package + announce to the tracker + actively push-seed the first replica** to an online receive-role holder when one exists (§2 push-seed), announce-and-wait otherwise; in a `require_approval` project the seed target is the **coordinator** and the history row shows `pending approval` until decided (§2). Publication history with per-package "delivered to k of m, held by …" (from the `{new,duplicate}` counts + friendly names, §7 Transfers).
- **Receive** (receive role only) — announced packages: available (sources online/offline) / downloading (progress, speed) / done (path, verified); per-project storage destination.
- **Overview** — members, roles, thresholds (read-only), totals, "Manage on portal" link.
- **Moderation** (coordinator only, shown only when `require_approval` is on; badge = pending count) — the approval queue (§2): pending package → contributor's frames (preview/blink + per-frame metrics from the manifest) → **Approve / Reject with reason**. The pending payload is already local (the publish push-seeded it to the coordinator), so review is on real data, offline-capable; the decision posts to the hub.

**Project ↔ object linking (explicit, not auto):** `project_links (project_id, frames_set_id)` — **local table, never sent to hub**. A picker ranks candidate frame sets by great-circle distance of set center to the project target (within radius first) with OBJECT-name similarity as a secondary signal; manual pick of any set allowed (warned if off-target — layer-1 precondition 4 still guards per frame). Multiple links per project (seasons/rigs); gate evaluates the union. Empty state: "Link an object to start". **Join-first-shoot-later:** when scanning/clustering creates or grows a set whose center falls within a linked project's target radius → discrete notification "Frame set matches project X — link it?"; never auto-link (user stays in control). Coordinator's source set auto-links on create-from-app. Unlinking drops candidates; published history is untouched.

**Received contributions — catalog boundary (important):** received calibrated frames carry `CALSTAT` + `ATH_CSRC`, and the scanner's reconcile-adopt would look for the source frame in the local catalog — which a processor doesn't have → eternal deferred-adopt warn. **This is now the same "self-describing file → special landing" machinery the mesh work already built** (Plan 2C light-cal reconcile-adopt, four branches known/moved/duplicate/adopt): Stage II adds a **sibling discriminator** — a file carrying calibration cards **plus a project provenance card (`ATH_PRJ` = project id, written at publish time)** routes to a new `project_contributions` table (frame metadata from the manifest) and **never enters `frames`/clustering/sessions/duplicates** — same philosophy as own B5 artifacts. Build it on top of the existing reconcile-adopt structure, not as a third bespoke path.
- **Landing reuses the mirror-landing path (Plan 2B):** `<CollabRoot>/<project-slug>/<member>/…` is the same shape as personal sync's `<incoming_root>/<sender-slug>/<rel_path>` — reuse `sync::ingest::land_payload` + `sanitize_slug` with a project+member prefix. **Load-bearing cross-account:** the member slug is resolved by the **receiver** from the authenticated peer id + its project-membership mapping — **never** from a path/name in the package (an untrusted member must not choose its own directory). This is Plan 2B's rule made essential.
- Their UI is the Receive tab, not Objects. A project-scoped WBPP export entry point for processors ("export combined dataset, calibration off") ships within Stage II — it is the processor's payoff.

**Transfers:** project exchange rides the same engine tables + Transfers surface from Stage I (a `project` dimension on queue/history rows); TransferIndicator aggregates both. Every serve to a peer is an outbound history row (what, to whom, when, bytes) — G3 extends to collaboration. A processor serving to several receivers is the **per-peer engine map** (one engine per destination, Plan 2C) — reuse as-is. **Accounting + holders UI feed off the Offer/Want `{new,duplicate}` finished counts** (Plan 3): a receiver's fetch reports how many frames were genuinely new vs already-held → "delivered to k of m", per-package holders, and the aggregate contribution accounting (§4/§9). **Holder/member names** resolve from the project-scoped **friendly-name resolver** (Plan 2C unique node names) — node id → member name for the "held by `<member>`" surface (here scoped to project membership, cross-account).

## 8. End-to-end flows

**Publish a project (coordinator):**

1. Entry points: in-app "Publish as project" on a frame set / Projects page → opens `projects.artfrom.space/new?object=…&ra=…&dec=…` prefilled from the catalog; or the portal form directly (manual target entry; SIMBAD name resolution = later nicety).
2. Form: target (name, RA/Dec, radius), title/description, integration goals (hours per filter, optional — feeds C9), initial thresholds (template defaults: FWHM ≤ 3.5″, ecc ≤ 0.6, trailed reject), **coordinator's own data role**, **`require_approval` toggle** (on → coordinator's role is forced to `send+receive`, §2), chat link (optional v1 field), data-policy template text (Q3).
3. Submit → live in the public directory → appears in the coordinator's app on next membership poll (coordinator badge); source set auto-linked. From here the coordinator contributes like any member.

**Join (participant):**

1. Discovery on the portal only (directory, shared links with Discord embeds); the app's "Browse projects" is a portal link.
2. "Request to join" (sign-in required; no account → email-OTP signup inline). Request carries **desired role** (contribute/process/both — coordinator still decides), short message, profile card.
3. Coordinator gets email + portal badge (the "70% decided in 7 days" metric lives on email nudges); approves with role assignment or rejects — on the portal.
4. Participant gets email; project appears in their app on next poll. First-open: receive role → storage-destination prompt (creates `<CollabRoot>/<project-slug>/`); everyone → link-object prompt, gate starts evaluating, transport picks up the membership snapshot.

**Leave/removal:** portal action → next snapshot excludes the node → serving/fetching stop (C8). Invite-by-link: Phase III backlog (BRD only requires join requests).

## 9. Notifications

Discrete outcomes only, `notify()` conventions; new kind `project` (+ Stage I's `sync`): "your contribution is replicated", "new package available in <project>", "download complete", "frame set matches project X — link it?", "join request approved"; approval flow (§2): "contribution awaiting your approval" (coordinator), "your contribution was approved" / "…rejected: <reason>" (contributor). Dedupe keys = package/project ids.

## 10. Stage III outlook (sketch, not design)

Discord bot (C-9): hub-driven private channel per project, invites via the OAuth-verified Discord link from D-2, milestone events posted. Profiles (A5): public pages on the portal, Discord nick shown verified. Stats (A4): imaging/transfer dashboards from aggregate reports; telemetry consent per Q8. Design these against real Stage II usage.

## 11. Testing

Gate: unit tests over fixture `frame_analysis` rows (each precondition + each rule + unit conversion + the binning gotcha). Delivery: multi-node swarm tests over `LoopbackTransport` (publisher offline after first replica; role enforcement; supersede; have-report convergence); **per-holder Offer/Want** (a partially-replicated receiver pulls only the missing frames; resume across a *different* holder with no duplication; a superseding new-content frame transfers, the old is resolved out); **push-seed** (contributor seeds one online receive-role holder, goes offline, swarm still completes); **project `PeerAuthorizer`** (a non-member / send-only node is refused a serve; membership-snapshot revocation stops serving next refresh). Cross-account (§2a): **a malicious package cannot steer landing** (a package-supplied slug/path is ignored — the receiver resolves the member slug + `validate_rel_path`-guards `rel_path`); a bad-content holder is caught by content-hash verify. Approval (§2): a `pending` package is never served to / listed for a non-coordinator member; approve → swarm serving begins; reject → contributor notified with the reason, package never serves, the coordinator's review copy is cleaned; a manual-approval project refuses a coordinator without `send+receive` at the API level (not just the form). Scanner: received-contribution branch built on the reconcile-adopt structure (with/without `ATH_PRJ`, dedupe on re-scan). Hub: API integration tests; snapshot signature verification. E2E (extends the Stage I harness): three instances — contributor, processor, coordinator — publish → push-seed/join → gated publish → swarm delivery → combined WBPP export.

## 12. Open items (carried, with owners)

- Q3 data-policy template text (owner + community) — needed for the publish form.
- Q6 minimal moderation (report button + hub-side hide + email contact) — before the directory is publicly announced.
- Q7 coordinator disappearance escalation — not blocking; revisit after real projects exist.
- Store-and-forward premium tier (Phase IV) — pricing/economics with Q2.
- **Parallel multi-source fetch** (§2 last bullet) — Stage-II *performance* follow-up after the sequential per-holder swarm ships; a fetch-scheduling layer over content-addressed blobs (split the want-set across online holders), not a protocol/format break. Owner + eng, post-v1.
- Delegated admin roles (project "moderator" who can approve contributions/joins) — Phase III backlog (§5a); design against real usage.

## 13. Build order (owner, 2026-07-12) — hub-first, five slices

Each slice is independently testable and reviewable; one implementation plan per slice. App-side work happens on branch **`0.5.0`** (0.4.0 is release-bound — beta.2 shipped 2026-07-12); hub-side in `athenaeum-hub` as usual.

1. **Hub project model** (C-1 extension) — §5/§5a tables + API, **signed membership snapshots** (the trust anchor everything else consumes), announcement state machine incl. approval, hub integration tests. No UI.
2. **Portal** (C-2) — SPA in the hub repo per §6: directory, project page, new-project form, coordinator admin, OTP sign-in, OG embeds.
3. **App project client, catalog side** (C-7 minus exchange) — membership poll, Projects page + cards + detail tabs, project↔object linking, **quality gate** (C-6) with the candidate table. No transfer yet.
4. **Exchange** — project-scoped `PeerAuthorizer`, publish → announce → push-seed (incl. the approval path + Moderation tab), per-holder Offer/Want swarm fetch, mirror landing + `ATH_PRJ` reconcile-adopt + `project_contributions`.
5. **E2E + payoff** — the three-instance E2E harness (§11), project-scoped WBPP export, notifications polish (§9).

Rationale: the exchange risk was retired by the mesh work (per the 2026-07-12 review — it assembles from shipped primitives); the genuinely new surface is the hub project model + portal, and the snapshot format they define is consumed by every later slice.
