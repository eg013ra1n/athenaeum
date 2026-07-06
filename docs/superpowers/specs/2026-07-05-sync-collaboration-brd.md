# BRD — Athenaeum Sync & Collaboration — 2026-07-05

**Status:** Draft for review. **Owner:** Vilen.
**2026-07-06 update:** design round closed Q1/Q4/Q5/Q9 (marked inline below) and amended one non-goal (cloud storage → tracker refinement). Designs: `2026-07-06-personal-sync-design.md` (Phase I), `2026-07-06-collaboration-projects-design.md` (Phase II).
**Scope note:** This is a *business requirements* document — it defines WHAT and WHY, not HOW. Technology is mentioned only where it is a product decision already made by the owner (Discord as communication platform); the P2P transport is **iroh** (decided 2026-07-06, see Q9). It supersedes the product-level assumptions of Pillar C in `2026-07-02-target-features-architecture.md` (which described a serverless, account-less model); the technical design there remains a candidate for the exchange layer and will be revised against this BRD. Schema groundwork (roadmap Phases 1/4: UUIDs, portable paths, change journal, packages) remains valid and necessary regardless of the design revision.

---

## 1. Problem Statement

Astrophotographers routinely operate more than one machine — a capture computer at a remote/backyard observatory and a primary computer holding the main library — and today Athenaeum treats each as an isolated island: material must be moved by hand, storage on the capture machine fills up, and there is no record of what was transferred. Separately, deep-sky imaging increasingly happens in *groups* (several imagers accumulating integration on one object), and there is no tooling that connects a personal FITS catalog to a group effort — coordination happens in ad-hoc chats, file exchange over generic cloud drives, with no quality control and no visibility of who contributed what.

Not solving this keeps Athenaeum a single-machine catalog. Solving it makes Athenaeum the backbone of both a personal multi-site imaging workflow and community imaging projects — a capability no direct competitor offers end-to-end.

## 2. Product Vision (one paragraph)

One Athenaeum **account** connects all of a user's machines. Capture machines automatically feed the primary library and clean up after themselves according to user-defined retention rules, keeping a full transfer history. Any user can **publish an object** as a collaborative project on the Athenaeum web portal; approved participants contribute quality-gated, calibrated data to each other over peer-to-peer transport, with roles (who sends, who receives) assigned by the project coordinator, progress visible on the portal, and discussion in a members-only project chat (Discord-based).

## 3. Goals

- G1. A user with N machines maintains **one** authoritative library with zero manual file ferrying: capture-to-home transfer requires no action in automatic mode.
- G2. Capture-machine storage becomes self-managing: retention policies keep disks from filling without losing unsynced data.
- G3. Every byte that leaves a machine is accounted for: users can always answer "what was transferred, when, and where is it now".
- G4. A collaboration project can go from "published" to "first accepted contribution" without any file leaving the participants' machines through a third-party cloud.
- G5. Contribution quality is enforced by the tool, not by trust: only frames passing the project's thresholds are publishable.
- G6. The central service gives each account an imaging profile: personal capture statistics and cross-project contribution history.

## 4. Non-Goals

- **Cloud storage of image data.** The central server never stores or proxies FITS/XISF payloads and never stores frame-level metadata; it coordinates. *(Amended 2026-07-06: it MAY store package-level announcements — ids, content hashes, sizes, counts, aggregate quality stats — i.e. a tracker role, enabling swarm delivery between members. Encrypted store-and-forward on private paid relays is a Phase IV candidate requiring a further amendment.)*
- **In-app chat.** Communication lives in Discord threads; the app/portal link to them. (Discord already does this better; building chat is a distraction.)
- **Real-time co-editing / shared live catalog.** Participants exchange files and metadata; each catalog stays autonomous. (Multi-writer sync is a different, much harder product.)
- **Processing-stage collaboration.** Exchange covers calibrated frames and their metadata; joint processing (shared stacks, versioned edits) is a possible future, not this scope.
- **Marketplace / payments / licensing management.** Projects are free community efforts in v1; data-rights questions are handled socially (see Open Questions).
- **Federation / self-hosted coordination servers** in v1: one central Athenaeum service. (Revisit if the community demands it.)

## 5. Personas

| Persona | Description |
| ---- | ---- |
| **Remote imager** | Owns a home computer (primary library) and one or more observatory capture machines. Wants capture material to flow home automatically and observatory disks to manage themselves. |
| **Project coordinator** | Approves members, sets quality thresholds, assigns roles, tracks progress and contribution quality via analysis data. Exactly one per project at any time. The creator is the initial coordinator and can hand coordination to another member (becoming a contributor or processor themselves). Coordination is a *hat*, not a data role: the coordinator also picks their own data role — send-only contributor or receive-everything processor. |
| **Contributor** | Joins projects to add integration time. Shoots, calibrates, analyzes locally; sends passing frames. May not want to receive anyone else's data. |
| **Processor** | Joins projects to process the combined dataset. Needs to receive everything from everyone; may contribute frames too. |

## 6. User Stories

### Personal multi-machine sync

- As a **remote imager**, I sign in to the same account on my home and observatory installs so that the system knows these machines belong together.
- As a **remote imager**, I designate my home library as *primary* and the observatory install as a *capture node* so that data flows in one defined direction.
- As a **remote imager**, I install the lightweight **Perseus** agent on the observatory machine instead of the full app, sign in with my account, and pick my home installation as the target so that the observatory box needs no catalog, no UI beyond setup, and minimal resources.
- As a **remote imager**, I enable automatic mode so that everything captured (lights **and** calibration frames) transfers to the primary library as-is, with no interaction.
- As a **remote imager**, I use manual mode after a night's local review so that only frames I select (e.g., after blink/analysis triage) are sent home.
- As a **remote imager**, I configure retention on the capture node — delete after successful transfer: immediately / after N days / when disk usage exceeds X% (oldest transferred files first) — so that the observatory disk never fills up, and nothing is deleted unless it has verifiably arrived at the primary.
- As a **remote imager**, I view transfer history on both machines so that I can confirm what was moved, when, and trace any file's origin.
- As a **remote imager**, I connect several observatories to one primary library so that all my rigs feed one catalog.
- As a **remote imager**, when a transfer is interrupted (power, network), it resumes without duplicating or losing frames, and nothing is deleted at the source until arrival is confirmed.

### Collaboration — coordinator

- As a **coordinator**, I publish an object from my catalog to the web portal (target, description, goals) so that others can discover and request to join.
- As a **coordinator**, I approve or reject join requests so that I control who participates.
- As a **coordinator**, I set per-project quality thresholds (FWHM, eccentricity, SNR, and other analysis metrics) so that only data meeting the bar becomes publishable to the project.
- As a **coordinator**, I assign each member a role — *send-only* or *send+receive* — so that data flows match responsibilities (shooters vs processors), and I choose my own role too.
- As a **coordinator**, I see per-member contribution progress (frames, integration time, filters) on the portal so that I can steer the campaign.
- As a **coordinator**, my project has a members-only chat (linked from the project page; auto-created private Discord channel later) so that all discussion happens in one place visible only to participants.
- As a **coordinator**, I monitor the quality of incoming contributions through their analysis metrics so that I can steer members toward the project's bar, not just gate them.
- As a **coordinator**, I hand coordination of the project to another member so that the project can outlive my availability; I then continue as a contributor or processor.

### Collaboration — participant

- As a **contributor**, I browse published objects on the portal and request to join so that I can add my integration to a shared target.
- As a **contributor**, only my frames that are calibrated, fully analyzed, and above the project thresholds are marked publishable so that I can't accidentally send junk.
- As a **contributor**, I see which of my frames failed which threshold so that I know why something isn't publishable.
- As a **processor**, I automatically receive all published contributions from all members so that I always process the complete dataset.
- As a **participant**, my profile card shows my Discord nick and links (AstroBin, Instagram, other socials) so that collaborators know who I am.
- As a **participant**, I leave a project (or am removed) and the system stops all further exchange with me so that membership is enforceable.

### Account & service

- As a **user**, I create one Athenaeum account and sign in on any install so that all features above attach to my identity.
- As a **user**, I keep using Athenaeum fully offline/without an account so that the core catalog never depends on the service. *(Hard requirement: account is additive, never a gate on existing features.)*
- As a **user**, I see my imaging statistics (per target, per night, volume transferred) in my account so that I get a cross-machine picture of my activity.

## 7. Requirements

### A. Accounts & central service

| # | Pri | Requirement |
| ---- | ---- | ---- |
| A1 | P0 | Single account, multiple device installs; sign-in from desktop app; devices listed and revocable in account settings. |
| A2 | P0 | All existing app functionality works with no account (local-only mode unchanged). |
| A3 | P0 | Central service authorizes accounts and distributes current relay lists to clients (enabling P2P connectivity across NATs). |
| A4 | P1 | Service records transfer statistics per account (volume, counts) and imaging statistics; visible to the account owner. |
| A5 | P1 | Public member profile: display name, Discord nick, AstroBin/Instagram/social links; owner-editable; shown on project pages. |
| A6 | P2 | Account deletion / data export (GDPR-shaped hygiene). |

**Acceptance sketch (A1/A2):** fresh install works fully without sign-in; after sign-in on two machines both appear in the device list; revoking one ends its participation in sync.

### B. Personal multi-machine sync

| # | Pri | Requirement |
| ---- | ---- | ---- |
| B1 | P0 | Machine roles per account: one *primary* library; any number of *capture nodes*, each linked to the primary. Transport: P2P, no third-party cloud in the path (candidate per Q9). |
| B1a | P0 | **Perseus** — a lightweight capture-node agent, separate from the full app: sign in with the Athenaeum account, pick the target Athenaeum installation (primary), point at the capture directory — it watches, packages, sends, applies retention, keeps history. No catalog, no library UI; runs headless/as a service on modest observatory hardware. Automatic mode only — manual triage-and-send (B3) remains a full-Athenaeum-as-capture-node feature, which stays supported. |
| B2 | P0 | Automatic mode: every newly scanned file on a capture node (all frame types) is queued and transferred to the primary as-is. |
| B3 | P0 | Manual mode: user selects frames/sets on the capture node and sends them explicitly; unselected material stays. Mode is per capture node. |
| B4 | P0 | Delete-after-transfer with verifiable delivery: a file is eligible for source deletion only after the primary confirms intact receipt. |
| B5 | P0 | Retention policies on capture node: (a) delete immediately on confirmation; (b) keep N days after transfer; (c) trigger at disk-usage ≥ X%, deleting oldest *transferred* files first. Never auto-delete an untransferred file. |
| B6 | P0 | Transfer history on both ends: what, when, from/to which machine; survives app restarts; searchable by filename/object. |
| B7 | P0 | Interrupted transfers resume; no duplicates in the primary catalog (same frame arriving twice = one catalog entry). |
| B8 | P1 | Frame metadata (user edits, analysis results, tags) travels with the file so the primary catalog doesn't re-derive from headers alone. |
| B9 | P1 | Conflict rule: primary wins — capture-node metadata never overwrites newer primary edits for an already-transferred frame. |
| B10 | P2 | Bandwidth scheduling (transfer windows, rate limits) for metered observatory links. |

**Acceptance sketch (B4/B5):** kill the network mid-transfer → file remains at source, not deleted; restore network → transfer completes, then (policy a) file is removed at source and history shows both events; fill disk past X% → only transferred files are removed, oldest first, untransferred files untouched.

### C. Collaboration projects

| # | Pri | Requirement |
| ---- | ---- | ---- |
| C1 | P0 | Publish an object from the app catalog to the portal: target, title, description, imaging goals; visible in a public project directory. |
| C2 | P0 | Join requests with coordinator approve/reject; membership list on the project page. |
| C3 | P0 | Data roles per member, set by the coordinator only: *send-only* / *send+receive*; the coordinator also sets their own data role. Role changes take effect for subsequent exchange. Coordination itself is a separate, transferable right (C3a), held by exactly one member at a time. |
| C3a | P1 | Coordination handover: the current coordinator can transfer coordination to any member; the former coordinator keeps (or re-picks) a data role. Exactly one coordinator at any moment; handover is recorded in project history. |
| C4 | P0 | Publishable-data gate: only frames that are (1) calibrated, (2) passed the full analysis cycle in the member's app, and (3) meet project thresholds (FWHM, eccentricity, SNR, extensible set) can be published to the project. Thresholds defined by the coordinator; per-frame pass/fail reasons visible to the member. |
| C5 | P0 | File exchange between members over the same P2P transport as personal sync (Q9), per assigned roles; the central service carries no image payloads. |
| C6 | P0 | Contribution accounting on the portal: per-member frames, integration time, filter split; project totals. |
| C7 | P1 | Project discussion space: every project has a members-only chat, linked from the project page and visible **to members only**. v1: a chat-link field the coordinator fills (any platform — Discord channel, Telegram, …). v2: auto-created private channel on the Athenaeum Discord server via bot at publish time. Public threads are explicitly not the mechanism (visible to the whole server, auto-archive on inactivity). |
| C8 | P1 | Leaving/removal: exchange with the departed member stops; already-transferred data handling per the project's data policy (see Open Questions Q3). |
| C9 | P1 | Coordinator dashboard: coverage vs goals (e.g., target hours per filter), member activity recency. |
| C10 | P2 | Tile-plan projects: a mosaic project splits into tiles that members claim (connects to the Mosaic pillar). |
| C11 | P2 | Project archive/completion state: coordinator marks a project done; the portal shows final results (link to processed image). |

**Acceptance sketch (C4):** coordinator sets FWHM ≤ 3.0″; a member's frame with FWHM 3.4″ is not publishable and shows "FWHM 3.4 > 3.0"; the same frame after re-analysis at 2.9″ becomes publishable.

### D. Portal (web)

| # | Pri | Requirement |
| ---- | ---- | ---- |
| D1 | P0 | Project directory (browse/search published objects) + project pages (description, members, progress, join button). |
| D2 | P0 | Sign-in with the same Athenaeum account. |
| D3 | P1 | Public profile pages (A5). |
| D4 | P2 | Personal imaging-statistics dashboard (A4 visualization). |

## 8. Success Metrics

Leading (first 60 days after each phase ships): ≥ 50% of multi-machine users enable sync (measure: accounts with ≥ 2 devices that activated a capture-node link); zero data-loss reports attributable to retention deletion (support channel); ≥ 10 published projects with ≥ 3 members each; ≥ 70% of join-request decisions within 7 days (proxy for coordinator engagement).

Lagging (2 quarters): share of active users signed in to an account; total data volume exchanged P2P (service stats); repeat collaboration rate (members who join a 2nd project); community growth on the Discord server. Targets TBD after baseline telemetry exists (see Q8).

## 9. Open Questions

| # | Question | Who | Blocking? |
| ---- | ---- | ---- | ---- |
| Q1 | **Closed 2026-07-06:** passwordless email OTP as the sole base sign-in (in-app, no browser); optional Discord OAuth *link* added with collaboration features (verified nick for Phase III bot). Rationale: personal-sync design §2. ~~Account/auth provider strategy for the service — product choice with UX and moderation implications.~~ | Owner | Closed |
| Q2 | Who operates the central service and Discord server, under what budget/SLA? Relay usage may carry traffic costs. **Candidate monetization:** subscription for *private per-project relays* (guaranteed bandwidth for a collaboration) — relay access gated by account/project via the central service's relay-list distribution (A3), so it needs no extra architecture. Licensing note: iroh(-relay) is MIT/Apache-2.0 (closed relay fork with auth is fine); Syncthing/strelaysrv is MPL-2.0 (running a paid service is fine; *distributing* a modified relay requires opening changed files). | Owner | Before Phase II |
| Q3 | Data policy for departed members and for the project as a whole: do contributions remain with the project? Who may publicly publish processed results, with what credit? Needs a simple written policy per project (template). | Owner + community | Before C8 |
| Q4 | **Closed 2026-07-06:** trust + stamp — gate evaluated locally; manifest carries metric values, engine version, analysis `config_hash`, calibration `ATH_CVER`, threshold-set version. Receiver-side re-validation possible later, not v1. Design: collaboration-projects §4. | Owner | Closed |
| Q5 | **Closed 2026-07-06:** calibrated-only in v1; manifest `payload_kind` reserves `raw_light`/`master` so a per-project raw+masters mode is a Phase IV extension, not a format break. Design: collaboration-projects §3. | Owner + community | Closed |
| Q6 | Portal moderation: public directory implies abuse/reporting/takedown flows — minimal viable moderation? | Owner | Before D1 public launch |
| Q7 | Coordinator *disappearance* (no voluntary handover per C3a): does the project freeze, or is there an escalation path (e.g., service admin reassigns after N months of inactivity)? | Owner | No |
| Q8 | Telemetry consent model for service statistics (A4) — opt-in vs on-by-default-with-account. | Owner | Before A4 |
| Q9 | **Closed 2026-07-06: iroh.** Decisive: NAT traversal rate + HTTPS/443 relay fallback; `iroh-relay` HTTP-callback auth plugs directly into the hub for the paid private-relay tier (Q2); MIT/Apache licensing; embeddable in the headless Perseus agent. Cost accepted: self-hosted relay infrastructure from day one. Validation folded into the Perseus-first MVP track; Syncthing remains the fallback behind the transport trait. Rationale: personal-sync design §2. | Owner + eng | Closed |

## 10. Phasing

Dependency-shaped, each independently valuable; maps onto roadmap Phases 4–6, which will be re-planned against this BRD:

- **Phase I — Personal sync** (A1–A3, B1–B7): one account, observatory→home flow, retention, history. No portal needed beyond account sign-up. *This alone is a headline feature.*
- **Phase II — Collaboration core** (C1–C6, D1–D2): publish, join, roles, quality gate, P2P exchange, progress accounting.
- **Phase III — Community layer** (C7–C9, A4–A5, D3): Discord threads, profiles, dashboards, statistics.
- **Phase IV — Extensions** (C10–C11, B10, D4): tile-plan projects, completion states, bandwidth scheduling.

Prerequisites from the existing engineering roadmap stay in force: catalog/frame UUIDs, portable paths, change journal, package export/import (`2026-07-02-roadmap.md` Phases 1 & 4) are the substrate for B6–B9 and all of C.
