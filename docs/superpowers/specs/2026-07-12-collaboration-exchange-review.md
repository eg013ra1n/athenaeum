# Collaboration (Stage II) — Exchange-Mechanism Review — 2026-07-12

**Purpose:** re-examine the collaboration exchange model (`2026-07-06-collaboration-projects-design.md` §2/§3/§7, BRD D-8, components C-4/C-5) against the P2P mechanisms built **after** it (mesh model Phase 1 — `2026-07-11-sync-model-phase1-design.md`; dedup handshake Plan 3 — `2026-07-12-sync-dedup-handshake-design.md`; Perseus workflow; app send UI). Those weren't available when Stage II was designed, but the collab design **explicitly reuses the Stage I engine/transport/package format** (D-6/D-7), which they reshaped. This review reflects what the new mechanisms make possible, what to reuse, what to amend, and the one capability the swarm still needs. **Analysis only — no design decision is final until owner review.**

---

## 1. Why review now

Stage II delivery (D-8) is "hub as tracker + members as swarm": publish → package-level announcements + have-reports on the hub → a receive-role member fetches the whole package from any online holder → receiver verifies + ingests + acks. It reuses the Stage I `SharingTransport` + package/manifest + engine as-is.

Between then and now, the engine/transport gained five primitives that sit exactly on the collab exchange path:

1. **Offer/Want dedup handshake (Plan 3)** — before `Announce`, two peers negotiate the exact frame subset the receiver lacks (sampling-hash → full-hash confirm), then only that subset is served (want-subset collection). Best-effort, P2P, hub-free.
2. **Account-wide allow-list `PeerAuthorizer` (Plan 2A/H1)** — a fail-closed cached set of authorized peer pubkeys; the receiver rejects a non-authorized node before any fetch.
3. **Explicit-target send + per-peer engines (Plan 2C)** — `enqueue_sync_selection(dest)` builds/holds one engine per destination `NodeId`; a sender pushes a selection to a chosen node.
4. **Mirror landing + receiver-resolved slug (Plan 2B)** — received files land at `<incoming_root>/<sender-slug>/<rel_path>`, the slug resolved by the **receiver** from the authenticated peer id (never from the package).
5. **Self-describing-file reconcile-adopt (Plan 2C light-cal)** — a file carrying provenance cards is routed out of normal ingestion into a tracking table by identity, four branches (known / moved / duplicate / adopt).

Plus the fan-out cleanup coordinator, `{new,duplicate}` batch outcome, unique node names + friendly-name resolution, and the capability model. The collab design predates all of them; several of its hand-rolled exchange assumptions are now first-class primitives.

---

## 2. Element-by-element: collab exchange → new mechanism

### 2.1 Swarm fetch (§2 "fetch from any online holder") ⇒ **Offer/Want per holder**

**Today's collab design:** a receiver fetches the *whole* package from one holder; dedup/resume come implicitly from content-addressing.

**Reflection:** the Offer/Want handshake is precisely the per-holder fetch primitive the swarm wants. A receiver contacting a holder should `Offer`/`Want`-negotiate first, then pull **only the frames it lacks** from that holder. This makes the swarm fetch:
- **Efficient** — a partially-replicated receiver (has half the project from an earlier session) pulls only the missing half from the next holder it reaches, instead of re-fetching the package.
- **Resumable across holders** — the want-set is recomputed against whatever the receiver already holds, so an interrupted transfer resumes from a *different* holder with no duplication (BRD B7 "resume without duplicating", C5's uuid/content dedup — now a negotiated fact, not a post-fetch cleanup).
- **Supersede-compatible** (§3) — a re-published frame (same `ATH_CSRC` uuid, new content/`ATH_CVER`) has a *different* content hash, so it lands in the `want` set (absent content) and transfers; the receiver then resolves uuid-supersede from the manifest exactly as designed. Offer/Want (content dedup) and manifest-supersede (identity resolution) compose cleanly.

**Recommendation:** amend §2 to specify per-holder Offer/Want as the swarm's fetch step. It's already built and best-effort (a holder that doesn't speak it → full-package fetch fallback).

### 2.2 Role enforcement (§2 "serve only to receive-role node ids") ⇒ **project-scoped `PeerAuthorizer`**

**Today's collab design:** the hub signs membership snapshots (node ids + data roles); a provider serves a blob only to node ids with receive role in a current snapshot.

**Reflection:** this is exactly the `PeerAuthorizer` primitive (Plan 2A) — a fail-closed cached allow-list checked before any serve/fetch — but fed by a **project-membership source** instead of the account device list. Where personal sync's `refresh_authorized_peers` caches "every device in my account," collab's caches "every receive-role node in each of my projects' current signed snapshots." Same mechanism, different feeder.

**Recommendation:** reuse `PeerAuthorizer`; add a project-membership authorizer keyed by project, refreshed from the signed snapshot on the poll cadence (§2). The fail-closed property (empty cache authorizes nobody until the first refresh) is the right default cross-account. **This is a security upgrade over the ad-hoc design** — the primitive already exists and is reviewed.

### 2.3 The asynchrony pain (§2 "keep this machine online until a processor holds it") ⇒ **explicit-target push-seed**

**Today's collab design:** pure pull — publish → announce → receivers pull. The contributor must stay online until at least one always-on processor pulls the data.

**Reflection:** the explicit-target send (Plan 2C) lets a contributor **actively push** the publication to a chosen online receive-role holder (a "swarm seed"), then go offline — instead of passively waiting to be pulled. The app can auto-pick an online processor from the snapshot (or the operator picks one via the same `SendToNodeDialog` pattern) and `enqueue_sync_selection`-style push the package to it. The "delivered to k of m — keep online" UX becomes "seeding to <processor>…" → "replicated — safe to go offline" much sooner, because the contributor drives the first replica rather than waiting for a poll-triggered pull.

**Recommendation:** add active seeding to the publish flow (§8) as the primary path when an online receive-role holder exists; fall back to announce-and-wait when none is online. This directly attacks the hardest product problem in §2 (hobby desktops off during the day).

### 2.4 Received-contribution landing (§7 `<CollabRoot>/<project-slug>/<member>/`) ⇒ **mirror landing + reconcile-adopt**

**Today's collab design:** received calibrated frames land under a managed root per project/member, tracked in `project_contributions`, never entering `frames`/clustering; the scanner gets a new branch keyed on an `ATH_PRJ` provenance card.

**Reflection:** the mesh work already built both halves of this:
- **`sync::ingest::land_payload` + `sanitize_slug`** (Plan 2B) lands at `<root>/<receiver-resolved-slug>/<rel_path>`. Collab's `<CollabRoot>/<project-slug>/<member>/` is the same shape with the slug = member (and a project prefix). Crucially, Plan 2B's rule — **the landing prefix is resolved by the receiver from the authenticated peer id, never from the package** — is *essential* cross-account (an untrusted member must not choose its own directory). Reuse it verbatim; the member-slug comes from the receiver's project-membership resolution.
- **Self-describing-file reconcile-adopt** (Plan 2C): a file carrying provenance cards is routed out of normal ingestion by identity. Collab's "CALSTAT + `ATH_PRJ` → `project_contributions`, never `frames`" is the *same* pattern as the light-cal reconcile-adopt (CALSTAT + `ATH_CSRC` → `light_calibrations`). The four-branch machinery (known/moved/duplicate/adopt) already exists; collab adds the `ATH_PRJ` discriminator and a project-contributions table.

**Recommendation:** build the collab received-contribution branch on top of the existing reconcile-adopt structure (a sibling discriminator + table), and reuse `land_payload`/`sanitize_slug` for the layout. Aligns two "self-describing file" code paths instead of a third bespoke one.

### 2.5 Data role (BRD C3 send / send_receive) ∥ **capability model**

**Reflection:** the mesh capability (`athenaeum` full-peer vs `perseus` send-only) is the same shape as collab's per-member data role (send_receive vs send-only). A collab contributor behaves like a send-only capability (never runs a project receiver, never a serve target); a processor like a full peer. The difference is **scope**: capability is account-global and self-asserted at registration; collab's role is **project-scoped and coordinator-assigned**, so it lives in the signed snapshot, not on the device. But the *enforcement* pattern is identical — "a send-only node is never a valid destination" (mesh `resolve_dest_node` rejects perseus) becomes "a send-only member is never served to."

**Recommendation:** model the collab data-role as a project-scoped capability in the snapshot; reuse the "send-only is never a destination" enforcement in the project authorizer. Don't overload the account-global capability with project semantics.

### 2.6 Multi-peer serving + `{new,duplicate}` accounting ⇒ **per-peer engines + fan-out coordinator + finished counts**

**Reflection:** a processor serving to N receivers is exactly the per-peer engine map + (if it fans one package to several) the cleanup coordinator built in Plan 2C/3 — reuse as-is. And the `{new,duplicate}` finished-event counts (Plan 3) directly feed collab's "delivered to k of m, held by …" and the aggregate contribution accounting (§4/§7): a receiver's fetch reports how many frames were genuinely new vs already-held. Friendly-name resolution + unique node names (Plan 2C) resolve holder node ids → member names for the "held by <member>" UI (here scoped to project membership, cross-account).

---

## 3. The one real gap: multi-**source** fetch

The swarm's headline promise — "any member holding a blob can serve it… data spreads from replicas" (§2) — implies a receiver assembling one package from **several holders in parallel** (BitTorrent piece selection). The new Offer/Want makes the *per-holder* content-diff possible, and content-addressing makes a blob fetchable from any holder, but the **engine still fetches a whole collection by `root_hash` from ONE provider** (`fetch_collection_to_dir`, one `EndpointId`). So today a receiver Offer/Want-negotiates with holder A and pulls its want-subset from A; to pull the rest from holder B it runs a second, separate negotiation+fetch against B.

That sequential-per-holder model is a correct and useful v1 (it already delivers resume-across-holders and dedup). True parallel multi-source — split the want-set across all currently-online holders and fetch blobs concurrently by hash — is the **one genuinely new engine capability the swarm wants that is not built**. iroh-blobs can fetch individual blobs by hash from any provider, so the substrate supports it; it needs a multi-provider fetch layer above the current single-provider `fetch`.

**Recommendation:** scope collab v1 to **sequential per-holder Offer/Want fetch** (buy the dedup/resume wins immediately, no engine rewrite), and record **parallel multi-source fetch** as the defined Stage-II-follow-up / performance pillar — not a format or protocol break, purely a fetch-scheduling extension over the same content-addressed blobs.

---

## 4. What does NOT carry over: the cross-account trust boundary

The mesh model's security posture is "**every device in my account is mine and mutually trusted**" — the allow-list is account-scoped and the peers are the same user's machines. **Collaboration is cross-account**: a project mixes different users' nodes. So the mesh model's *trust assumption* does not transfer, and several things get **more** load-bearing:

- **The authorizer's trust anchor is the hub-signed snapshot, not account membership.** A member is served only if in a current signed snapshot; leaving/removal (C8) = next snapshot excludes them. Snapshot signature verification is the trust root (already in the §11 test list).
- **Receiver-resolved everything.** Plan 2B's "the landing slug is resolved by the receiver, never taken from the package" and Plan 3's "the sender's `rel_path` is `validate_rel_path`-guarded on both sides" go from prudent (same-account) to **essential** (untrusted member). A malicious member's manifest must never steer a path, a slug, or a dedup skip.
- **The Plan-3 "empty-Want → Confirmed" note becomes a real cross-account concern.** Same-account it was "a lower-effort variant of forging Duplicate acks, within the trust boundary." Cross-account, a malicious *receiver* answering "I want nothing" can't cause a *sender* to lose data (the sender keeps its raw sources — §3 "raw sources never leave the contributor"), but a malicious *holder* serving wrong content is caught by content-hash verification on fetch (C-4 "content-hash-verified"). Enumerate these in the collab threat model; the mesh primitives already verify content on the wire, which is the defense.
- **Quality-gate stamps travel for later receiver-side re-validation (§4, Q4).** Cross-account, the ability to re-validate a contributor's claimed metrics (values + engine version + config_hash + threshold-set version in the manifest) matters more than same-account. Built later, but the stamps must travel from v1 — they already do.

**Recommendation:** carry the mesh primitives *and* their receiver-side validation into collab, but re-anchor trust on the signed snapshot + content-hash verification, and write a short cross-account threat model (the mesh security note explicitly deferred cross-account to "the separate Stage II collaboration design" — this is where it lands).

---

## 5. Bottom line + recommended amendments

The new P2P work is **strongly additive** to collaboration — most of the collab exchange design's hand-rolled pieces are now reviewed primitives, and one product-hard problem (asynchrony) gets a direct new tool (push-seed). Net: the collab exchange gets simpler to build and safer, with one bounded new capability (multi-source) deferred.

**Amendments to fold into the collab design when Stage II is planned:**
1. **§2 swarm fetch** → per-holder **Offer/Want** (only-missing transfer, resume-across-holders, supersede-compatible). *Reuse Plan 3.*
2. **§2 role enforcement** → project-scoped **`PeerAuthorizer`** fed by the signed snapshot (fail-closed). *Reuse Plan 2A.*
3. **§8 publish** → **active push-seed** to an online receive-role holder (explicit-target send), announce-and-wait as fallback. *Reuse Plan 2C.*
4. **§7 received-contribution** → build on **mirror landing + reconcile-adopt** (receiver-resolved member slug; `ATH_PRJ` as a sibling self-describing-file discriminator). *Reuse Plan 2B/2C.*
5. **Data role** → project-scoped **capability** in the snapshot; "send-only is never a destination" enforcement. *Reuse the capability pattern.*
6. **Accounting/holders UI** → fed by `{new,duplicate}` finished counts + friendly-name resolution. *Reuse Plan 3/2C.*
7. **New pillar:** **parallel multi-source fetch** (Stage-II follow-up; not v1; not a protocol break).
8. **New section:** **cross-account threat model** — re-anchor trust on the signed snapshot + on-wire content-hash verification; receiver-resolved paths/slugs are now essential; enumerate the malicious-member cases.

**Sequencing note:** none of this changes the fact that Stage II depends on the hub project model (C-1) + portal (C-2) being built. The exchange layer, though, is now mostly assembled from shipped, reviewed primitives — the swarm is Offer/Want + a project authorizer + push-seed over the per-peer engine, not a new transport.
