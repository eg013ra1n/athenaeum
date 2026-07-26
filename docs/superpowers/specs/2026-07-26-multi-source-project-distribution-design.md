# D3 — Multi-source project distribution

**Status:** design approved 2026-07-26 (brainstorm with owner: auto-replication confirmed; per-project toggle default ON; pending on-demand only; hub carries root hash — delivered at publish, see §3.2 amendment) · **Audit:** `docs/superpowers/research/2026-07-25-delivery-model-audit.md` (F6) · **Predecessors:** D1 (peer reachability), D2 (receive-side waiting), W1 (upload cap), W2 (parallel receiving + queued vocabulary).

## 1. Problem

Projects are collaboration between astrophotographers who publish **analyzed, calibrated** frames for a shared stack. Two defects keep that from working at scale:

**(a) A file held by four participants downloads at one participant's uplink.** The download loop walks the hub's holder list strictly sequentially (`collab_exchange.rs:1008`): 5 s probe per holder, `ProjectRequest` to ONE, then a 90 s silent-death window (`WAIT_LOCAL_COMPLETE`, `:482-483`) before the next holder starts a fresh transfer. The fetch itself is single-provider by construction — `fetch_collection_to_dir` takes a scalar `provider` (`blobs.rs:328`) and wraps it in `Shuffled::new(vec![provider])` (`:347`, `:442`), and `Downloader::download` hardcodes `SplitStrategy::None` (vendored `downloader.rs:404-416`). `online_count` is computed, stored, and never consulted by the loop.

**(b) The swarm never forms.** Publish seeds exactly ONE member — "the FIRST eligible member's first node" (`api/collab.rs:1069-1094`). Everyone else pulls manually, whenever they remember to. A package's holder set is usually {publisher, maybe coordinator}, so even a perfect multi-source fetch would have one and a half sources to use.

iroh-blobs 0.103 already ships the machinery (a) needs, unused: `SplitStrategy::Split` fans a collection out **per child blob** (= per frame) across the full provider set, up to 32 children concurrently (`downloader.rs:180-195`, `:440-482`), with sequential-resume failover inside each child — "if the first provider had the first 10% … it will only ask the next provider for the remaining 90%" (`:486-490`). `ContentDiscovery` is one method, blanket-implemented for `Vec<EndpointId>` (`:557-572`). The blocker is two lines of our code plus the missing root hash.

## 2. Decisions (owner-confirmed)

| Decision | Choice |
| ---- | ---- |
| Where the swarm comes from | **Auto-replication**: every `send_receive` member's device automatically downloads published, non-superseded contributions |
| The need list | **Computed locally as a diff** — `published ∧ ¬superseded ∧ ¬mine ∧ local_status ≠ complete`. Nothing travels; the hub's announcement list is already the shared truth |
| Auto-replication scope | **Per-project toggle, default ON**, with the project's published byte total shown next to it. Local preference (column on `collab_projects`), never hub state |
| Pending packages (coordinator) | **On-demand only** — moderation metrics already arrive hub-side without a download; frames-by-eye is an explicit button. Rejected work never costs disk |
| Root hash to the hub | **(а), implemented at PUBLISH — zero hub-side work.** Plan-time verification found the entire pipe already exists (`package_announcements.root_hash` NOT NULL on the hub, POST field, list field, `AnnouncementWire.root_hash`, `project_packages.root_hash` synced at poll) but carries a manifest-bytes IDENTIFIER (`api/collab.rs:1299-1303` says so itself). The fix is the VALUE: publish imports the collection first (which is also its first seed, §3.4) and sends the REAL collection hash. Publisher attestation holds by construction — only the publisher can create the announcement. Fallback (б) remains for legacy-value announcements |
| Multi-source fetch | `SplitStrategy::Split` over the hub holder list (minus self), phantoms tolerated by failover |
| Seeding | Downloaded packages re-serve under the reserved `project/…` tag namespace, imported with **`ImportMode::TryReference`** (verified present and honored by the fs store) — the blobs reference the landed files in place, no double storage |
| `send`-role devices | Auto-replication does not apply (authz already forbids them pulling, `collab_exchange.rs:911-914`); they seed their own packages only |
| Perseus | Out of scope — no projects, no receiver in that crate |

## 3. Architecture

### 3.1 The swarm fetch

`download_project_package` (`api/collab_exchange.rs:893`) gains a second, preferred path, taken when the announcement carries a root hash:

1. **Providers** = hub holders (`AnnouncementWire.holders`, `hub_client.rs:155-156`) minus self, each mapped `pubkey → NodeId` and given a relay-only dial hint (`node.add_peer` with the holder's `relay_url` — the S1 cross-account rule stays: relay only, never direct addrs). No pre-probing, no ranking: `Shuffled` re-shuffles per child request (`downloader.rs:587-588`), which load-spreads for free, and `execute_get`'s per-provider failover absorbs phantoms and corpses at the cost of one failed connect each, not 5–90 s.
2. **Fetch**: a new `fetch_collection_multi(providers: Vec<EndpointId>, root_hash, …)` beside today's scalar fn — same two-phase shape (meta first, then payload), but phase 2 goes through `download_with_opts` with `SplitStrategy::Split`. The existing in-flight GC tag (`in_flight_tag`) is set the same way, so partial data survives restarts and re-fetches resume across ANY holder (content addressing makes the bytes interchangeable).
3. **Telemetry surfaced, not dropped**: `TryProvider` / `ProviderFailed` / `PartComplete` progress items (currently `_ => {}` at `blobs.rs:466`) feed (i) a per-package `sync_events`-style journal line per provider transition and (ii) a live "downloading from N sources" figure on the project package row. Vocabulary reuses W2's: a package waiting for the auto-worker's turn reads `queued`.
4. **Ingest**: unchanged — the fetched staging dir goes through `ingest_project_package` on `spawn_blocking` with `IngestConn::Shared` (per-frame locking, W2). No ack is sent on this path: acks settle a *sender's* outbound row, and no holder enqueued one — `report_have` is this path's completion signal. The fallback path (б) keeps its ack exactly as today.
5. **Fairness**: the multi-fetch takes ONE `ReceiveGate` permit (same gate as personal sync, `sync.max_concurrent_receives`) for its whole fetch+ingest — a project pull is a receive like any other. Named follow-up from W2 stands: the project permit has no mid-fetch abort path.

**Not changed:** `handle_project_announce`/`ProjectRequest` (the push-shaped path) stays intact — it IS the fallback, and the push-seed at publish still uses it.

### 3.2 The real root hash at publish (no hub-side work)

Plan-time verification overturned the brainstorm's premise that the hub lacked a hash field. The full pipe exists: hub `package_announcements.root_hash` (NOT NULL, 64-hex-validated), required on the create POST, returned by the announcements list, mirrored in `AnnouncementWire.root_hash` (`hub_client.rs:144`), persisted into `project_packages.root_hash` at poll (`api/collab_exchange.rs:615`). Today's value is a BLAKE3 of the manifest bytes — an identifier, explicitly not the collection hash (`api/collab.rs:1299-1303`: "This is an IDENTIFIER only; the wire transfer substitutes the real iroh collection hash per serve").

- **Publish changes the value, not the pipe**: import the written package dir as a collection FIRST (`ImportMode::TryReference` under `project/<project_id>/<package_id>` — this is the publisher's first-seed step from §3.4, just moved before the hub POST), then send the returned collection hash as `root_hash`. The announcement is born swarm-capable; no window, no follow-up report.
- **Attestation by construction**: only the publisher can create the announcement (hub auth on POST), so the hash every downloader verifies against is publisher-controlled — the same trust surface as the content itself. No hub-side acceptance rule needed.
- **Legacy discrimination**: an old announcement's `root_hash` is a manifest identifier that no provider's blob store contains. The swarm path simply TRIES it — every provider fails the GET within one connect round — then falls back to (б) and caches a per-package "swarm-unfit" verdict in memory for the rest of the session, so legacy packages cost one cheap failed round per app run, not per retry.
- The hub's `valid_root_hash` (64 hex) accepts a real collection hash unchanged. **No hub commit, no hub deploy** — D3 ships entirely app-side.

### 3.3 The auto-replication worker

App-side background loop (core, both hosts — same lazy-init shape as other sync machinery):

- **Trigger**: every `COLLAB_AUTO_SYNC_INTERVAL` (20 min), plus immediately after a hub poll that changed any project's package set, plus a per-project "Sync now" button. Each pass: refresh announcements (existing `refresh_project_packages`), compute the need diff per auto-enabled project, download missing packages **one at a time per project** (the Split fan-out inside one package already saturates the link; cross-package parallelism would just fight the ReceiveGate).
- **Need diff** (pure function, unit-tested): `state == published && !superseded && origin != mine && local_status != complete && role allows`. A `failed` package re-enters the diff on the next pass — retry-by-cadence, no ladder (the swarm fetch already absorbed per-holder failures; a whole-swarm failure means every holder is gone, and 20 minutes is an honest retry interval for that).
- **Toggle**: `collab_projects.auto_replicate INTEGER DEFAULT 1` + the project page control with the published-bytes figure. `set_project_auto_replicate` command, both backends.
- Downloads run under the existing `local_status` lifecycle (`downloading`/`complete`/`failed`) so the project page needs no new state machine.

### 3.4 Seeding under `project/…`

After a successful ingest (and after own publish), the device imports the package's serve dir as a collection tagged `project/<project_id>/<package_id>` with `ImportMode::TryReference`:

- **Seed dir is a hardlink reconstruction, not the live serve tree** (implementation correction, 2026-07-26/T4): TryReference into the `collab_serve` staging dir is fatal — that tree is torn down by `CollabCleanupSink` after export, leaving the references dangling (ENOENT on the next GET, demonstrated in a red test). Seeds are built from `collab_seed/<package_id>` dirs of hardlinks to the landed files (`reconstruct_seed_dir`); hardlinks also keep superseded inodes alive until the seed itself is deleted.
- **Disk cost — publisher ≈ zero, downloader pays a second copy** (measured, 2026-07-26/T4): for the PUBLISHER, the fs store references the payload files in place (verified: `ImportMode::TryReference` in vendored `api/proto.rs:630-644`, honored by `store/fs/import.rs`; the store may fall back to copying tiny files — fine, the payloads are FITS). For a DOWNLOADER, the swarm fetch already ingested the bytes as store-OWNED blobs, and a later TryReference import does not displace an owned entry — so a seeding downloader carries the landed files PLUS the store's owned copy for as long as the seed tag pins them. Reclaiming that duplicate is a named follow-up (§7). The invariant TryReference demands — the file never changes after import — holds by construction: contributions live in the managed collab root, never enter `files`/`frames`, and the app never edits them.
- The namespace is already contract-reserved and sweep-proof (`node.rs:274-284`; the orphan sweep provably skips `project/…` — pinned by the existing foreign-tag test). Seeding tags are permanent for as long as the package's contributions exist locally; deleting a project's local data deletes its `project/<id>/…` tags in the same operation (the ONE place both sides are torn down together).
- With blobs resident, an incoming iroh GET from another member's swarm fetch is served by the provider machinery with no control-message round trip — this is what makes the puller-driven swarm work. `report_have` after ingest (existing hook) is what advertises it.
- W1's upload pacer caps seeding egress device-wide — an observatory that seeds a popular package cannot lose its SSH.

### 3.5 What the user sees

- Project page, per package: holder count (exists) + "downloading from N sources" while fetching + W2's `queued` chip while waiting for the worker/gate.
- Project page, per project: the auto-replication toggle with the published-bytes total; "Sync now".
- No Transfers-page rows for project pulls in v1 (collab has its own surface; folding the two lists is a separate UX decision).

## 4. Failure honesty

- All holders phantom/offline → the fetch fails after one failover cycle through the provider set (seconds, not 5–90 s per holder), `local_status = failed` with a per-provider reason line, re-tried next worker pass. No infinite in-pass retry.
- A holder dying mid-child costs that child a provider switch with byte-level resume; the package completes from the survivors.
- Publisher offline before its first have → hash-less announcement → fallback (б) → if THAT fails too (sole holder gone), failed-with-reason, next pass. Nothing new to invent: this is today's failure, just labeled.

## 5. Interactions with the existing model

| Existing piece | Interaction |
| ---- | ---- |
| `ReceiveGate` (W2) | one permit per package pull; auto-worker serializes per project anyway |
| Upload pacer (W1) | caps seeding egress; no per-project knob in v1 |
| Per-peer lanes (W2) | untouched — the swarm path doesn't ride the receiver lanes at all; fallback (б) does, as today |
| `queued` vocabulary | reused for worker-queued packages |
| `have` monotonicity | unchanged; phantoms tolerated by failover; "no un-have" stays a hub follow-up |
| Pending gating (`authz.rs:83-94`) | unchanged — swarm fetch is only ever attempted for `published` |
| Push-seed at publish | unchanged (first member still gets an immediate push); auto-replication makes it a bootstrap, not the distribution |

## 6. Security

- **Root hash is publisher-attested by construction.** It travels only in the announcement-create POST, which the hub already authenticates as the publisher. Every downloader fetches whatever content the hash names and verifies AGAINST THAT HASH — BLAKE3 makes the transfer tamper-proof, and the hash naming the right content reduces to trusting the publisher, who already controls the content. Holder-attested hashes (the rejected have-carrier variant) would have let any member redirect the whole project's downloads.
- Relay-only dial hints for cross-account holders (S1) — unchanged, and the swarm path uses the same rule per holder.
- The connect gate (`node_in_any_project`) and serve-side role checks (`authz.rs:83-94`) already govern who can pull blobs; the swarm changes how many sources a puller uses, not who may be one.

## 7. Out of scope

Perseus. Un-have on the wire (hub follow-up, unchanged). Seeding retention/cleanup policy beyond project-data deletion (a "stop seeding but keep files" control is a named follow-up). Reclaiming a downloader-seed's duplicate store copy (§3.4 — e.g. drop the owned entry and re-import the landed files as references once the fetch settles). Moderation preload for coordinators (declined — on-demand button stays). Folding project pulls into the Transfers page. Mid-download provider refresh via a live `ContentDiscovery` stream (the fixed per-pass holder snapshot is enough at project scale; noted as a cheap later upgrade since `find_providers` is re-called per child).

## 8. Testing

- **Need diff**: pure-fn unit tests — superseded excluded, mine excluded, failed re-enters, role-gated, toggle-gated.
- **Multi-provider fetch**: real-QUIC localhost harness (the `sharing/iroh/tests.rs` two-endpoint pattern, extended to three): two providers serve the same package, one puller fetches with Split — assert completion + content hash + BOTH providers saw traffic (per-provider telemetry is the oracle). The failover pin: kill one provider mid-transfer (drop its transport) → fetch completes from the survivor; assert wall-clock stays far under the old 90 s window.
- **TryReference seeding**: import a landed dir, assert the store serves the collection AND the blob dir did not grow by the payload size (publisher-side only — a downloader's store already owns the bytes from the fetch, §3.4); assert the `project/…` tag survives the orphan sweep (extends the existing foreign-tag pin).
- **Real hash at publish**: the POSTed `root_hash` equals the imported collection hash (pinned by comparing against a direct `import_package_collection` of the same dir); a LEGACY-value announcement (providers lack the hash) falls back to (б) cleanly and caches the swarm-unfit verdict (second attempt in the same session goes straight to fallback).
- **Worker**: auto-enabled project with 2 missing packages downloads both serially and reports have; toggle off → no downloads; `send`-role → no downloads.
- Existing collab e2e (slice tests) stay green — the fallback path is byte-identical to today.

## 9. Risks

- **TryReference's immutability assumption** is load-bearing: if anything ever rewrites a landed contribution in place, seeds serve garbage that fails BLAKE3 verification at every downloader (fail-loud, not silent corruption — but the seed looks like a phantom). The collab root is app-managed; the risk is a user hand-editing files there. Accepted: verification makes it a availability problem, never an integrity one.
- **Auto-replication default ON** means joining a project starts downloads. Mitigated by the visible byte total, the per-project toggle, and the fact that invited members joined precisely to get the data. The ReceiveGate + upload pacer bound the resource impact.
- **No hub coupling at all** (the brainstorm expected some): the hub pipe pre-exists and validates only the hash's shape. The one cross-version seam is legacy announcements carrying identifier values — handled by try-then-fallback with a cached verdict, one cheap failed round per package per session.
