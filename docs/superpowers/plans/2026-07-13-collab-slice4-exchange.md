# Collab Slice 4 — Exchange (Publish / Swarm / Moderation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A project member can publish gate-passing calibrated frames (announce to the hub + push-seed the first replica), a receive-role member can download any announced package from any online holder (sequential per-holder request-to-serve; full serve + ingest-level dedup — see Д2), received contributions land in the collaboration root + `project_contributions` (never `frames`), and a coordinator moderates pending contributions in the app. Spec §2/§2a/§3/§7/§8 + build-order §13 slice 4.

**Architecture:** Everything assembles from shipped Stage-I/mesh primitives (per the 2026-07-12 exchange review): the per-peer `SyncEngine` map drives both push-seed and holder serves (Offer/Want is deliberately skipped for project packages — the Д2 manifest anchor requires byte-identical manifests; dedup happens at ingest via Duplicate receipts); landing reuses the Plan-2B mirror shape; the scanner gains an `ATH_PRJ` sibling of the light-cal reconcile-adopt. New pieces: two appended `Msg` variants (`ProjectAnnounce`, `ProjectRequest`), an appended `ManifestRecord.project` stamp, a project-scoped authorizer fed by the cached verified snapshots (`collab/authz.rs`, ungated), byte-level `ATH_PRJ` FITS stamping (`fits_writer::stamp_extra_card`, no pixel decode), two catalog tables (`project_packages`, `project_contributions`), publish/download/moderation orchestration in `api/collab.rs` (render-gated) with the receive/serve path ungated, and the Contribute/Receive/Moderation UI. Manifest authenticity vs a malicious holder is anchored by `manifestXxh3` inside the hub announcement's `aggregateStats` (package-level — the hub never sees frame data).

**Tech Stack:** Rust (rusqlite, postcard wire, iroh + iroh-blobs via the existing `sharing` layer, wiremock + LoopbackTransport for tests), React 18 + TS (ts-rs types), Tailwind design tokens.

## Global Constraints

- **Repo/branch:** athenaeum repo, continue on branch **`0.5.0`** (slice-3 tip `d04eb0ca`). No version bumps.
- **Two backends in sync (house law):** every new Tauri command (`crates/athenaeum-tauri/src/commands/collab.rs`) ships with its Axum mirror (`crates/athenaeum-web/src/routes/collab.rs`) in the same commit; real logic in `athenaeum-core`.
- **Feature-gating law:** `sharing`/`package`/`sync`/`collab`/`db` stay **ungated** (Perseus headless build). Everything the *receiver/serve* path needs is ungated. Publish orchestration + moderation live in render-gated `api::collab` (they need the gate). Gate for every task: `cargo build -p perseus --no-default-features` stays green.
- **Wire-compatibility law (postcard):** `Msg` is a positional postcard enum — new variants are appended at the END, existing variants and their field order are NEVER touched. `PackageAnnounce`/`FrameReceipt`/etc structs are NEVER extended (positional break). `ManifestRecord` is JSON (forward-compatible, no `deny_unknown_fields`) — new fields are appended with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- **Cross-account trust anchors (spec §2a, load-bearing):** (1) landing prefix (project slug + publisher slug) is resolved by the RECEIVER from hub-anchored data (the project row + the announcement's publisher), never from the package; (2) every peer-supplied `rel_path` passes `package::validate_rel_path`, every wire `package_id` passes `validate_package_id`; (3) the manifest bytes are verified against the hub-anchored `manifestXxh3` before ANY record is trusted; (4) per-frame payloads are verified against the (now-anchored) manifest `xxh3`; (5) only hub-fetched verified snapshots feed the project authorizer (never peer-relayed data); (6) fail-closed everywhere — no row/list/dir means refuse.
- **Snapshot contract (BINDING, hub README):** apply every signature-verified snapshot (compare content, not version); nodes ordered by raw pubkey bytes; `supersedes` = **announcement ids** (not package ids), own-in-project only.
- **Hub API (built, slice 1 — exact wire):** `POST /projects/{uuid}/announcements` body `{packageId: Uuid, rootHash: 64-hex, byteSize>0, frameCount>0, aggregateStats: object, supersedes: [announcementId]}` → `{id, state: "pending"|"published"}`; born `pending` iff `require_approval && !coordinator`. `GET /projects/{uuid}/announcements` → `[{id, packageId, publisherDisplayName, own, rootHash, byteSize, frameCount, aggregateStats, supersedes, state, rejectReason, createdAt, decidedAt, holders: [{pubkey, displayName, lastSeenAt}]}]` (member sees published+own; coordinator sees all). `POST /announcements/{id}/approve` / `/reject` body `{reason: 1..=500 chars}` (coordinator; 409 when not pending). `POST /announcements/{id}/have` → 204 (DEVICE bearer required; role send_receive or coordinator; pending holdable only by coordinator). Errors: bare-status 401/403/429, `{"error": msg}` for 400/404/409.
- **Serde boundary:** new wire DTOs `#[serde(rename_all = "camelCase")]` + `#[derive(ts_rs::TS)]`, registered in `ts_export.rs` `decls![]`; regenerate `src/types/models.ts` via `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; never hand-edit.
- **Never swallow errors** — `tracing` before returning; command wrappers wear `#[tracing::instrument(skip_all, err)]` (web: `err(Debug)`).
- **Events:** transfers ride the existing `sync-progress`/`sync-finished` events (they gain a `project_id: Option<String>` field — in-process + JSON payloads, safe to extend); discrete outcomes notify via `notify()` kind `project` with package/project dedupe keys.
- **Tests:** LoopbackTransport for multi-node exchange tests; wiremock for hub HTTP; `let (_tmp, ctx) = test_ctx();` tuple fixture for api-layer tests (tempdir FILE db). Slice gates: `cargo build --workspace`, `cargo test -p athenaeum-core`, `cargo test -p perseus`, `cargo build -p perseus --no-default-features`, `npx tsc --noEmit`.
- **Commit identity:** `eg013ra1n <vilen.sharifov@gmail.com>`, never a Claude author/co-author line.

## Design decisions (Д1–Д10 — deltas/refinements vs the spec text, for owner review)

- **Д1 — Swarm fetch = "request-to-serve".** The engine is sender-driven, so a receiver pulls by ASKING a holder to push: new `Msg::ProjectRequest{project_id, package_id}`. The holder authorizes the requester (project authorizer + pending→coordinator-only), reconstructs the package dir from its local copy, and enqueues an explicit-target send to the requester through the existing per-peer engine — receipts, history, confirm semantics reused verbatim. Sequential per-holder: the requester tries hub-listed holders one at a time with a delivery timeout. **Identity note (audit B1):** the engine mints a FRESH wire `PackageId` per serve (`engine.rs:1333-1335`), so the HUB package id travels explicitly — in `ProjectStamp.package_id` and on the `ProjectAnnounce` wire variant. Receivers key `project_packages` rows on the hub id; acks/fetches keep using the wire `announce.package_id` (that's what the serving engine correlates on, `engine.rs:944`).
- **Д2 — Manifest authenticity anchor.** The hub announcement's free-form `aggregateStats` carries `manifestXxh3` (xxh3-64 of the exact `manifest.ndjson` bytes). A receiver verifies the fetched manifest against it BEFORE trusting any record — closing the "malicious holder re-authors manifest + content consistently" hole that per-frame xxh3 alone cannot (package-level datum, so the BRD "no frame-level metadata on hub" amendment holds). Every holder must therefore serve the byte-identical original manifest — `project_packages.manifest_ndjson` retains it. **Consequence (audit B2): project serves are ALWAYS full** — the engine skips the Offer/Want negotiation for project packages (`want = None`), because a negotiated subset re-serializes a FILTERED manifest (`import_subset_collection`) whose bytes can never match the anchor. Transfer-level dedup for collab is deferred to the parallel-multi-source follow-up (same fetch-scheduling layer); v1 dedup happens at ingest — identical `(uuid, xxh3)` re-deliveries produce `Duplicate` receipts and land nothing.
- **Д3 — `ATH_PRJ` stamping is a byte-level header edit, not a re-encode.** Our calibrated artifacts are simple single-HDU FITS written by `write_fits_f32`. `fits_writer::stamp_extra_card` copies the file inserting one 80-byte card before `END` (using the last header block's padding slot when free, else growing the header by one 2880-byte block and streaming the data after it). No pixel decode → ungated, fast, deterministic. The LOCAL artifact stays unstamped; the stamped copy is the published payload.
- **Д4 — Publications are retained, not cleaned.** The publish package dir survives under `<sync_dir>/collab_pub/<package_id>/` (exempt from engine cleanup via a no-op cleanup sink) so the contributor can re-serve as a holder and re-seed. Deleted on reject or when its announcement is superseded by a later own-publication.
- **Д5 — Publisher slug is hub-anchored.** Landing is `<CollabRoot>/<project-slug>/<publisher-slug>/<rel_path>` where publisher-slug = `sanitize_slug(announcement.publisherDisplayName)` from the HUB list (per-package), never from the package or the serving peer (the server may be a third-party holder). Project-slug = `sanitize_slug(collab_projects.slug)`.
- **Д6 — v1 download is explicit.** The Receive tab lists announced packages; "Download" starts the sequential holder loop. "New package available" notifications come from the poll. Auto-fetch is a recorded follow-up, not v1.
- **Д7 — Connect-time intercept goes live.** The mesh left iroh's connect-intercept unused (finding F5, same-account residual). Cross-account it becomes load-bearing: the receiver transport now rejects connections from node ids that are neither account devices nor members of any cached project snapshot (composite check, fail-closed on empty). Per-package/pending granularity stays at the serve/request layer; residual (a published-capable member fetching a pending blob by hash) is documented — non-coordinators never learn pending root hashes from the hub.
- **Д8 — Push-seed target selection.** `require_approval` pending → coordinator's nodes only; else the snapshot's send_receive members' nodes (own nodes excluded), first candidate enqueued. The engine's own retry/backoff IS the announce-and-wait fallback (no separate mechanism); UX derives from outbound state (`queued/announced` = "waiting for a holder", `confirmed` = "replicated — safe to go offline").
- **Д9 — Supersede computation.** Publishing frames whose `frame_uuid`s appear in my own earlier non-rejected announcements for this project → those announcement ids go into `supersedes`. On success, local rows for the superseded packages are marked superseded (kept for history). Receivers resolve uuid-supersede at ingest: same `(project, frame_uuid, publisher)` replaces the older contribution row + landed file.
- **Д10 — (superseded by the Д2 consequence, audit B2).** Originally: teach the dedup responder `project_contributions` sampling hashes. Dropped — project serves skip Offer/Want entirely (full serve + ingest-level `Duplicate` receipts), so no responder change, no `sampling_hash` column. Revisit together with parallel multi-source fetch.

## Task overview (12)

1. Wire + manifest + FITS-stamp primitives (ungated): `Msg::{ProjectAnnounce,ProjectRequest}`, `TransportEvent` variants, loopback support, `ManifestRecord.project`, `fits_writer::stamp_extra_card`.
2. Hub client extension: announcements/decide/have wire (`collab/hub_client.rs` + DTOs + wiremock).
3. Tables + db layer (ungated): `project_packages`, `project_contributions`, `db/collab_exchange.rs`.
4. Project authorizer + transport intercept (ungated): `collab/authz.rs`, composite connect gate, receiver announce gate.
5. Project ingest (ungated): manifest-anchor verify, landing, contribution rows, receipts/ack/history + project dimension, uuid-supersede replace.
6. Serve reconstruction + request-to-serve (ungated): `serve_dir` rebuild from landed files + retained manifest, holder handler, collab sender map (dedicated blob store, retain-aware cleanup sink).
7. Publish build + orchestration (render-gated `api/collab.rs`): stamped package build, aggregate stats + `manifestXxh3`, hub announce, supersedes, push-seed, publication rows.
8. Download orchestration + announcements poll: sequential holder loop, package-state sync into `project_packages`, have-report, notifications data.
9. Moderation orchestration (render-gated): pending review data, approve/reject + review-copy cleanup on reject.
10. Scanner `ATH_PRJ` reconcile branch (ungated).
11. Command wiring both backends + ts_export + events project dimension + Transfers history column.
12. Frontend: Contribute publish UI + publication history, Receive tab, Moderation tab, storage prompt, Transfers project chip, notifications.

---

### Task 1: Wire + manifest + FITS-stamp primitives (ungated)

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/proto.rs` (append two `Msg` variants)
- Modify: `crates/athenaeum-core/src/sharing/types.rs` (two `TransportEvent` variants)
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (two trait methods with failing defaults)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (impl + accept-side dispatch)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs` (route both messages)
- Modify: `crates/athenaeum-core/src/package/manifest.rs` (`ProjectStamp` + `ManifestRecord.project`)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (project-aware announce in `negotiate_and_build`/announce path)
- Create: `crates/athenaeum-core/src/fits_writer/stamp.rs`; Modify: `crates/athenaeum-core/src/fits_writer/mod.rs` (declare + re-export)

**Interfaces:**
- Consumes: `Msg` postcard enum (`proto.rs:66`), `TransportEvent` (`types.rs:65`), `SharingTransport` (`sharing/mod.rs:39`), iroh accept loop (`iroh/mod.rs:611`), loopback mailboxes, `ManifestRecord` (`manifest.rs:33`), `format_card` (`fits_writer/card.rs:151`), `CARD_SIZE = 80`.
- Produces (BINDING for Tasks 4–9):
  - `pub struct ProjectStamp { pub project_id: String, pub package_id: String, pub thresholds_version: Option<i64>, pub cal_engine_version: Option<i64> }` (serde camelCase, Clone, Debug, PartialEq) — `package_id` is the HUB package uuid, minted at publish time (audit B1: the wire `PackageId` is engine-minted per serve and correlates acks only; the hub id is the row key everywhere else). `ManifestRecord.project: Option<ProjectStamp>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `Msg::ProjectAnnounce { project_id: String, package_id: String, announce: PackageAnnounce }` (`package_id` = hub id) and `Msg::ProjectRequest { project_id: String, package_id: String }` (hub id — the holder's row key) — appended LAST, existing variants untouched.
  - `TransportEvent::ProjectAnnounceReceived { from: NodeId, project_id: String, package_id: String, announce: PackageAnnounce }` and `TransportEvent::ProjectRequestReceived { from: NodeId, project_id: String, package_id: String }`.
  - `SharingTransport::announce_project(&self, to: NodeId, project_id: &str, package_id: &str, announce: &PackageAnnounce) -> Result<()>` and `request_project(&self, to: NodeId, project_id: &str, package_id: &str) -> Result<()>` — default impls `bail!("transport does not support project exchange")`.
  - `pub fn stamp_extra_card(src: &Path, dest: &Path, card: &Card) -> Result<(), FitsWriteError>` in `fits_writer` (re-exported from `mod.rs`).

- [ ] **Step 1: Manifest extension + failing test.** In `manifest.rs` add (below `PayloadKind`):

```rust
/// Stage-II project provenance stamp (slice 4). Appended, optional — absent
/// for personal-sync packages; forward-compatible (manifest is JSON).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStamp {
    pub project_id: String,
    /// HUB package uuid (announcement correlation key — audit B1). The wire
    /// `PackageId` is engine-minted per serve and only correlates acks.
    pub package_id: String,
    /// Threshold-set version the frames passed (spec §4 Q4 stamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds_version: Option<i64>,
    /// Light-calibration engine version of the payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cal_engine_version: Option<i64>,
}
```

and on `ManifestRecord`: `#[serde(default, skip_serializing_if = "Option::is_none")] pub project: Option<ProjectStamp>,` (LAST field). Fix EVERY existing struct literal with `project: None` — the complete list (audit m3): production `crates/perseus/src/run.rs:234`, `crates/athenaeum-core/src/api/sync.rs:821`; tests `perseus/src/run.rs:1870,2280`, `perseus/src/web.rs:2117`, core `sharing/iroh/tests.rs:93,949`, `sharing/tests.rs:263`, `sync/ingest_tests.rs:93,127,237,281,803`, `sync/engine_tests.rs:59,1130`, `package/tests.rs:39` (`package/tests.rs:202` uses struct-update syntax — safe). `manifest.rs` has NO tests module today — add one with a `sample_record()` fixture as part of this step. Test:

```rust
#[test]
fn project_stamp_roundtrips_and_absent_field_parses() {
    let mut r = sample_record(); // the fixture helper added in this step
    r.project = Some(ProjectStamp { project_id: "p-1".into(), package_id: "pkg-1".into(), thresholds_version: Some(3), cal_engine_version: Some(1) });
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"projectId\":\"p-1\""));
    let back: ManifestRecord = serde_json::from_str(&s).unwrap();
    assert_eq!(back.project, r.project);
    // v1 personal-sync line (no `project` key) still parses:
    let legacy = s.replace(&format!(",\"project\":{}", serde_json::to_string(r.project.as_ref().unwrap()).unwrap()), "");
    let old: ManifestRecord = serde_json::from_str(&legacy).unwrap();
    assert!(old.project.is_none());
}
```

- [ ] **Step 2: Wire variants.** `proto.rs`: append to `enum Msg` (AFTER the last existing variant, comment `// Slice-4 collab exchange — appended; postcard indices of the variants above are frozen.`):

```rust
    /// Provider advertises a PROJECT package (collab exchange, slice 4).
    /// `package_id` is the HUB package uuid (row key); `announce.package_id`
    /// stays the engine-minted wire id (ack correlation).
    ProjectAnnounce { project_id: String, package_id: String, announce: PackageAnnounce },
    /// A receive-role member asks a holder to serve a project package (hub id).
    ProjectRequest { project_id: String, package_id: String },
```

`types.rs`: append the two `TransportEvent` variants (in-process enum, no wire concern). `sharing/mod.rs`: add the two trait methods with `bail!` defaults (mirror `negotiate_want`'s default at `sharing/mod.rs:87`).

- [ ] **Step 3: iroh impl.** In `iroh/mod.rs`: `announce_project` mirrors `announce` (`iroh/mod.rs:401`) — same served-hash substitution from the `served` map, sends `Msg::ProjectAnnounce` via `send_control`; `request_project` sends `Msg::ProjectRequest` via `send_control`. In `SyncControlProtocol::accept` add two arms mirroring the `Announce` arm (`iroh/mod.rs:635-644`): forward `ProjectAnnounceReceived`/`ProjectRequestReceived` events, then the 1-byte delivery ack.

- [ ] **Step 4: loopback impl.** In `loopback.rs` mirror how `announce` routes to the peer inbox → event, for both new methods (`fetch` path unchanged — project packages fetch the same collections). Ensure `LoopbackTransport`'s event mapping emits the two new `TransportEvent` variants.

- [ ] **Step 5: engine project-awareness.** In `engine.rs::negotiate_and_build` (`engine.rs:726`), after reading the manifest records, capture the stamp: `let stamp = records.iter().find_map(|r| r.project.clone());` and store `(project_id, hub_package_id)` on the `Pending` slot (new fields `project_id: Option<String>`, `hub_package_id: Option<String>`). **Project packages SKIP the Offer/Want negotiation entirely** (Д2/audit B2): when the stamp is present, do not call `negotiate_want` — proceed as the full-send fallback does (`want = None`; counts `(total, 0)` like the existing fallback at `engine.rs:746-764`). At the announce call site (`engine.rs:649-658` region), branch: stamp present ⇒ `transport.announce_project(peer, &pid, &hub_pkg, &announce)`, else `transport.announce(peer, &announce)`. Also update the OTHER exhaustive `TransportEvent` match in the engine (`engine.rs:886` — no wildcard arm; audit m1) with no-op arms for the two new variants. Thread `project_id` into `emit_progress`/`emit_finished` payloads as a new `project_id: Option<String>` field on `SyncProgressEvent`/`SyncFinishedEvent` (`receiver.rs:74`/`receiver.rs:90` — JSON events, additive field, both already derive TS and sit in ts_export; regenerate TS in Task 11).

- [ ] **Step 6: `stamp_extra_card`.** Create `fits_writer/stamp.rs`:

```rust
//! Byte-level FITS header stamping: copy a simple single-HDU FITS file,
//! inserting ONE extra card before END. Our own `write_fits_f32` outputs are
//! the only intended inputs (single HDU, 2880-byte header blocks). No pixel
//! decode — the data region is streamed verbatim.
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use super::card::{format_card, Card, FitsWriteError, CARD_SIZE};

const BLOCK: usize = 2880;

pub fn stamp_extra_card(src: &Path, dest: &Path, card: &Card) -> Result<(), FitsWriteError> {
    let mut reader = BufReader::new(File::open(src).map_err(FitsWriteError::Io)?);
    // Read header blocks until the one containing END.
    let mut header: Vec<u8> = Vec::with_capacity(BLOCK * 2);
    let mut end_at: Option<usize> = None;
    while end_at.is_none() {
        let mut block = [0u8; BLOCK];
        reader.read_exact(&mut block).map_err(FitsWriteError::Io)?;
        let base = header.len();
        header.extend_from_slice(&block);
        for i in (0..BLOCK).step_by(CARD_SIZE) {
            if &block[i..i + 8] == b"END     " {
                end_at = Some(base + i);
                break;
            }
        }
        if header.len() > BLOCK * 64 {
            return Err(FitsWriteError::Malformed("no END card in the first 64 header blocks".into()));
        }
    }
    let end_at = end_at.expect("loop exits only with END found");
    let new_records = format_card(card)?; // CONTINUE-capable: may be several 80-byte records
    let needed = new_records.len() * CARD_SIZE;
    // Insert before END; header must stay 2880-aligned (grow by whole blocks as needed).
    let used_after = end_at + CARD_SIZE + needed; // cards incl. END after insertion
    let new_header_len = used_after.div_ceil(BLOCK) * BLOCK;
    let mut out: Vec<u8> = Vec::with_capacity(new_header_len);
    out.extend_from_slice(&header[..end_at]);
    for rec in &new_records {
        out.extend_from_slice(rec);
    }
    out.extend_from_slice(b"END");
    out.resize(out.len() + (CARD_SIZE - 3), b' '); // pad END record to 80
    out.resize(new_header_len, b' ');               // pad header to block boundary
    let tmp = dest.with_extension("tmp-stamp");
    {
        let mut w = BufWriter::new(File::create(&tmp).map_err(FitsWriteError::Io)?);
        w.write_all(&out).map_err(FitsWriteError::Io)?;
        std::io::copy(&mut reader, &mut w).map_err(FitsWriteError::Io)?; // data region verbatim
        w.flush().map_err(FitsWriteError::Io)?;
    }
    std::fs::rename(&tmp, dest).map_err(FitsWriteError::Io)?;
    Ok(())
}
```

(Audit-verified: `FitsWriteError` HAS `Io(std::io::Error)` but NO `Invalid` variant (`card.rs:9-21`) — add one variant `Malformed(String)` to the existing enum following its shape for the no-END case; do NOT create a new error type. `CardValue`'s string variant is `Str`, not `Text` — audit M3.) Re-export in `fits_writer/mod.rs`: `mod stamp; pub use stamp::stamp_extra_card;`.

- [ ] **Step 7: tests.**

```rust
// fits_writer/stamp.rs tests
#[test]
fn stamped_copy_parses_with_new_card_and_identical_data() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.fits");
    let data: Vec<f32> = (0..16).map(|v| v as f32).collect();
    let cards = vec![Card::new("ATH_TEST", crate::fits_writer::CardValue::Integer(7)).unwrap()];
    write_fits_f32(&src, 4, 4, 1, &data, &cards).unwrap();
    let dest = dir.path().join("b.fits");
    stamp_extra_card(&src, &dest, &Card::new("ATH_PRJ", crate::fits_writer::CardValue::Str("proj-uuid".into())).unwrap()).unwrap();
    let src_bytes = std::fs::read(&src).unwrap();
    let dest_bytes = std::fs::read(&dest).unwrap();
    // data region identical — compare the first 64 data bytes right AFTER each
    // header (the tail is zero padding on both files, a vacuous compare):
    let data_start = |b: &[u8]| (0..b.len()).step_by(2880).find(|&o| b[o..].chunks(80).take(36).any(|c| c.starts_with(b"END "))).map(|o| o + 2880).unwrap();
    let (s0, d0) = (data_start(&src_bytes), data_start(&dest_bytes));
    assert_eq!(&src_bytes[s0..s0 + 64], &dest_bytes[d0..d0 + 64]);
    // stamped header contains both keywords:
    let head = String::from_utf8_lossy(&dest_bytes[..dest_bytes.len() - 16 * 4]);
    assert!(head.contains("ATH_PRJ") && head.contains("ATH_TEST"));
    // header stays block-aligned:
    assert_eq!(dest_bytes.len() % 2880, 0);
}
```

Loopback wire test (in `sharing/loopback.rs` tests or `sync/engine_tests.rs`): package whose manifest carries a `ProjectStamp` → enqueue → assert the receiving inbox observes `TransportEvent::ProjectAnnounceReceived { project_id == "p-1", .. }` (not plain `AnnounceReceived`); and `request_project` → `ProjectRequestReceived` at the target.

- [ ] **Step 8: gates + commit.** `cargo test -p athenaeum-core fits_writer sharing sync 2>&1 | grep 'test result'`, `cargo test -p perseus`, `cargo build -p perseus --no-default-features`, `cargo build --workspace`. Commit: `feat(collab): project wire variants, manifest stamp, byte-level FITS card stamping`.

### Task 2: Hub client — announcements/decide/have (ungated collab module)

**Files:**
- Modify: `crates/athenaeum-core/src/collab/hub_client.rs`

**Interfaces:**
- Consumes: existing `CollabClient` (Task-1 slice 3: `new/url/net/get_json` idioms, `my_projects/project_page/membership_snapshot/thresholds/collab_pubkey`), hub wire per Global Constraints.
- Produces (BINDING for Tasks 7–9):

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceRequest {
    pub package_id: String,          // uuid string
    pub root_hash: String,           // 64-hex
    pub byte_size: i64,
    pub frame_count: i32,
    pub aggregate_stats: serde_json::Value,
    pub supersedes: Vec<String>,     // announcement ids
}
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceResponse { pub id: String, pub state: String }
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderWire { pub pubkey: String, pub display_name: String, pub last_seen_at: Option<String> }
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementWire {
    pub id: String, pub package_id: String, pub publisher_display_name: String, pub own: bool,
    pub root_hash: String, pub byte_size: i64, pub frame_count: i32,
    #[serde(default)] pub aggregate_stats: serde_json::Value,
    #[serde(default)] pub supersedes: Vec<String>,
    pub state: String, pub reject_reason: Option<String>,
    pub created_at: String, pub decided_at: Option<String>,
    #[serde(default)] pub holders: Vec<HolderWire>,
}
impl CollabClient {
    pub async fn announce(&self, token: &str, project_id: &str, req: &AnnounceRequest) -> Result<AnnounceResponse, AccountClientError>;
    pub async fn list_announcements(&self, token: &str, project_id: &str) -> Result<Vec<AnnouncementWire>, AccountClientError>;
    pub async fn approve_announcement(&self, token: &str, announcement_id: &str) -> Result<AnnounceResponse, AccountClientError>;
    pub async fn reject_announcement(&self, token: &str, announcement_id: &str, reason: &str) -> Result<AnnounceResponse, AccountClientError>;
    pub async fn report_have(&self, token: &str, announcement_id: &str) -> Result<(), AccountClientError>; // 204
}
```

POST bodies via the client's existing post idiom (mirror how `account/client.rs` posts + maps статусы: 200/204 ok, 401→Unauthorized, else read `{"error":..}` best-effort into the Network arm). Paths: `/projects/{id}/announcements`, `/announcements/{id}/approve|reject|have`.

- [ ] **Step 1:** wiremock tests FIRST (in `hub_client.rs` tests): announce happy-path (assert serialized camelCase body incl. `aggregateStats.manifestXxh3` passthrough and `supersedes`), list decodes holders + tolerates unknown fields, reject sends `{"reason": ...}`, have returns 204 → Ok(()), 403 body-less maps to the client's forbidden/Network arm without panicking. Hub edge cases the client must surface distinctly (audit m5, all verified hub-side): announce 409 on a CLOSED project and on a DUPLICATE packageId (globally UNIQUE); `supersedes` ≤100 + hub-side dedup ⇒ pre-dedupe client-side; `have` with a non-device token ⇒ 400 `{"error"}` (not 401/403); reject reason bound is BYTES 1..=500. Run: expect compile failures (methods missing).
- [ ] **Step 2:** implement DTOs + methods per the binding block. Run focused: `cargo test -p athenaeum-core --lib collab::hub_client 2>&1 | grep 'test result'` → all green.
- [ ] **Step 3:** gates (`cargo build -p perseus --no-default-features` — module is ungated) + commit `feat(collab): hub client announcements/decide/have wire`.

### Task 3: Catalog tables + `db/collab_exchange.rs` (ungated)

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (two tables, appended before `Ok(())` next to the slice-3 block)
- Create: `crates/athenaeum-core/src/db/collab_exchange.rs`; Modify: `crates/athenaeum-core/src/db/mod.rs` (`pub mod collab_exchange;`)

**Interfaces:**
- Consumes: slice-3 `collab_projects` (project_id TEXT PK, slug, members_json, require_approval…), house idioms from `db/collab.rs` (SELECT_COLS const + index-based row mapper).
- Produces (BINDING): the DDL below verbatim; row structs + functions listed below.

DDL (exact):

```sql
-- Stage II collaboration (slice 4): known packages + received contributions.
CREATE TABLE IF NOT EXISTS project_packages (
    package_id      TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL,
    announcement_id TEXT NOT NULL UNIQUE,
    publisher_display TEXT NOT NULL,
    own             INTEGER NOT NULL DEFAULT 0,
    root_hash       TEXT NOT NULL,
    byte_size       INTEGER NOT NULL,
    frame_count     INTEGER NOT NULL,
    manifest_xxh3   TEXT,                          -- hub-anchored (aggregateStats), NULL when publisher pre-dates Д2
    aggregate_stats TEXT NOT NULL DEFAULT '{}',
    supersedes      TEXT NOT NULL DEFAULT '[]',    -- JSON array of announcement ids
    state           TEXT NOT NULL,                 -- pending | published | rejected (hub-mirrored)
    reject_reason   TEXT,
    superseded      INTEGER NOT NULL DEFAULT 0,    -- set when another announcement supersedes this one
    origin          TEXT NOT NULL,                 -- 'mine' | 'received' | 'remote'
    local_dir       TEXT,                          -- retained publish dir (origin='mine')
    manifest_ndjson BLOB,                          -- exact manifest bytes (mine + fully-received) for re-serving (Д2)
    local_status    TEXT NOT NULL DEFAULT 'none',  -- none | downloading | complete | failed
    holder_count    INTEGER NOT NULL DEFAULT 0,    -- captured from the hub list at poll time (Task 8 writes, Task 11 reads)
    online_count    INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    decided_at      TEXT,
    fetched_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_project_packages_project ON project_packages(project_id);

CREATE TABLE IF NOT EXISTS project_contributions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id      TEXT NOT NULL,
    package_id      TEXT NOT NULL REFERENCES project_packages(package_id) ON DELETE CASCADE,
    frame_uuid      TEXT NOT NULL,
    publisher_display TEXT NOT NULL,
    rel_path        TEXT NOT NULL,
    landed_path     TEXT NOT NULL UNIQUE,
    byte_size       INTEGER NOT NULL,
    xxh3            TEXT NOT NULL,                 -- full-content, manifest-anchored
    frame_meta      TEXT NOT NULL DEFAULT '{}',
    analysis        TEXT,
    superseded      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_project_contributions_project ON project_contributions(project_id);
CREATE INDEX IF NOT EXISTS idx_project_contributions_uuid ON project_contributions(frame_uuid);
```

Functions (all `anyhow::Result`, `&Connection` first arg; SELECT_COLS idiom): `upsert_package(&PackageRow)`, `get_package(package_id) -> Option<PackageRow>`, `get_package_by_announcement(announcement_id)`, `list_packages(project_id) -> Vec<PackageRow>`, `set_local_status(package_id, status)`, `set_manifest(package_id, bytes)`, `mark_superseded(announcement_ids: &[String])`, `delete_package(package_id) -> usize` (CASCADE clears contributions), `insert_contribution(&ContributionRow)`, `contributions_for_package(package_id)`, `contributions_for_project(project_id)`, `replace_contribution_for_uuid(project_id, publisher, frame_uuid) -> Option<String /*old landed_path*/>` (deletes the older row for uuid-supersede, returns its path for file removal), `own_active_announcement_ids_for_uuids(project_id, uuids: &[String]) -> Vec<String>` (Д9 supersede computation: own + state != 'rejected' + not superseded packages containing any of the uuids).

- [ ] **Step 1:** failing tests first (in-memory conn + `init_db`): package upsert/roundtrip + list ordering (created_at DESC), contribution insert + CASCADE on delete_package, `replace_contribution_for_uuid` returns the old path and removes the row, `own_active_announcement_ids_for_uuids` picks only own+active packages containing the uuids (fixture: 3 packages — own-active-with-uuid, own-rejected-with-uuid, foreign-with-uuid → exactly the first's announcement id).
- [ ] **Step 2:** implement DDL + module. Full `cargo test -p athenaeum-core --lib db::collab_exchange` green; full core suite green (schema additive).
- [ ] **Step 3:** gates + commit `feat(collab): project_packages + project_contributions tables and access layer`.

### Task 4: Project authorizer + connect gate + fail-closed announce gate (ungated)

**Files:**
- Create: `crates/athenaeum-core/src/collab/authz.rs`; Modify: `crates/athenaeum-core/src/collab/mod.rs` (`pub mod authz;`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (optional connect gate on the accept side)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (ProjectAnnounceReceived arm — fail-closed skeleton)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (composite gate wiring at receiver start)

**Interfaces:**
- Consumes: `collab_projects.members_json` (serialized `Vec<SnapshotMember>` — `accountId/displayName/dataRole/coordinator/nodes[b64 32-byte]`, slice 3), `NodeId = [u8;32]`, `db::collab::list_projects_raw`-style reads, the F5 intercept region `iroh/mod.rs:198-208`.
- Produces (BINDING):

```rust
// collab/authz.rs — ALL reads are from the locally-cached, signature-verified
// snapshots (hub-fetched only, slice-3 invariant). Fail-closed: no row/parse
// error/no match => None/false.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberIdentity { pub display_name: String, pub data_role: String, pub coordinator: bool }
/// The member (if any) that `node` belongs to in `project_id`'s cached snapshot.
pub fn member_for_node(conn: &rusqlite::Connection, project_id: &str, node: &crate::sharing::types::NodeId) -> Option<MemberIdentity>;
/// True when `node` appears in ANY cached project snapshot (connect gate feed).
pub fn node_in_any_project(conn: &rusqlite::Connection, node: &crate::sharing::types::NodeId) -> bool;
/// May `node` be SERVED `package`? send_receive or coordinator; a pending
/// package only to the coordinator. Fail-closed on unknown package/project.
pub fn may_serve_package(conn: &rusqlite::Connection, project_id: &str, package_pending: bool, node: &crate::sharing::types::NodeId) -> bool;
/// May an inbound PROJECT announce from `node` for `project_id` be accepted?
/// (any current member role — send-only contributors push-seed).
pub fn may_accept_announce(conn: &rusqlite::Connection, project_id: &str, node: &crate::sharing::types::NodeId) -> bool;
```

Node matching decodes each member's `nodes[]` base64 into 32 bytes and compares to `node` (never compare base64 strings). Malformed entries are skipped with `tracing::warn!` once per call.

- [ ] **Step 1: failing tests** (in `authz.rs`, in-memory conn + `init_db` + a `collab_projects` row whose `members_json` holds two members: coordinator send_receive with node A, member send with node B): `member_for_node` finds A and B with right roles and None for unknown node; `may_serve_package(published)` true for A, false for B (send-only), false for unknown; `may_serve_package(pending)` true ONLY for A; `may_accept_announce` true for both A and B, false for stranger; `node_in_any_project` true/false accordingly; empty table ⇒ all false (fail-closed).
- [ ] **Step 2:** implement; focused tests green.
- [ ] **Step 3: connect gate.** `IrohTransport` gains `pub fn set_connect_gate(&self, gate: Arc<dyn Fn(&NodeId) -> bool + Send + Sync>)` (stored behind a mutexed `Option`). AUDIT NOTE (m6): the "F5 intercept region" at `iroh/mod.rs:198-208` is a COMMENT, not a hook — the router is a plain `Router::builder().accept(...)` (`:214-217`). The real mechanism: (a) gate at the TOP of `SyncControlProtocol::accept` (`iroh/mod.rs:611` — reject before decoding any `Msg`), and (b) wrap the blobs handler in a thin gating `ProtocolHandler` newtype that checks `connection.remote_id()` against the gate before delegating to the inner `iroh_blobs` handler. Gate absent ⇒ accept-all (today's behavior — Perseus and sender transports unchanged); gate refuses ⇒ close the connection, `tracing::warn!(from, "connection refused by connect gate")`. CONTRACT: an ungated peer gets no blob bytes and no control dispatch.
- [ ] **Step 4: composite wiring + announce-gate skeleton.** AUDIT NOTE (M2): the receiver transport is built INSIDE `SyncRuntime::ensure_started` (`receiver.rs:403`, transport at `:440-448`; the only production `SyncReceiver::spawn` caller is `receiver.rs:452`) — api/sync.rs has NO spawn call sites, only two `ensure_started` callers (`api/sync.rs:333`, `:374`). Introduce a `ReceiverHooks` struct (connect gate + the Task-5/6/8 hooks, all `Option`, `Default`) as ONE new `ensure_started` parameter; update the two api/sync.rs callers (passing the composite gate, other hooks `None` for now) and the ~8 test spawn/start sites (`tests/sync_e2e.rs:249,524`; `sync/ingest_tests.rs:165,487,665,862,937,995`) with `Default::default()`. Build the composite gate in `api/sync.rs`: `account allow-list contains node (existing SYNC_AUTHORIZED_PEERS logic) || collab::authz::node_in_any_project(&conn, node)` and `set_connect_gate` it. In `receiver.rs`'s event loop add the `TransportEvent::ProjectAnnounceReceived { from, project_id, announce }` arm: run `validate_package_id`, then a new `project_gate: Option<Arc<dyn Fn(&NodeId, &str) -> bool + Send + Sync>>` closure (threaded through `SyncReceiver::spawn` like the existing `PeerAuthorizer`; wired in `api/sync.rs` to `may_accept_announce`); unauthorized or gate-absent ⇒ `tracing::warn!` + `continue` (fail-closed). Authorized announces are, FOR THIS TASK, logged `info!(project_id, package_id, "project package announced — ingest lands in Task 5")` and dropped. `ProjectRequestReceived` likewise logged+dropped (Task 6).
- [ ] **Step 5:** loopback test: unauthorized project announce is dropped (no event past the gate — assert via log-capture or by the absence of staged files), authorized one reaches the info! path. Gates + commit `feat(collab): project authorizer, connect gate, fail-closed project announce gate`.

### Task 5: Project ingest — landing + contributions + receipts (ungated)

**Files:**
- Create: `crates/athenaeum-core/src/sync/project_ingest.rs`; Modify: `crates/athenaeum-core/src/sync/mod.rs`
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (ProjectAnnounceReceived: fetch + ingest + ack + events)
- Modify: `crates/athenaeum-core/src/sync/ingest.rs` (make `sanitize_slug` `pub(crate)` visible to the sibling — it already is; reuse `unique_path` by making it `pub(crate)` if private)

**Interfaces:**
- Consumes: Task-3 db layer, Task-4 gates, `package::{read_manifest, validate_rel_path, xxh3_full_file, MANIFEST_FILENAME}`, `db::scan_root_path_of_kind(conn, "collaboration")`, `sanitize_slug`, receipts/history idioms from `ingest.rs` (`FrameReceipt`, `ReceiptOutcome`, `insert_history_row`, `Direction::Received`).
- Produces (BINDING):

```rust
pub struct ProjectIngestOutcome { pub receipts: Vec<crate::sharing::types::FrameReceipt>, pub ok_count: usize, pub failed: Vec<String> }
/// Ingest a fetched PROJECT package from `staging_dir`. Fail-closed: the
/// package row (with its hub-anchored manifest_xxh3) must already exist.
pub fn ingest_project_package(
    conn: &rusqlite::Connection,
    staging_dir: &std::path::Path,
    project_id: &str,
    package_id: &str,            // HUB package uuid (the project_packages row key, audit B1)
    peer_device: &str,           // authenticated serving peer (history only — NOT the landing slug)
) -> anyhow::Result<ProjectIngestOutcome>;
```

Algorithm (each numbered item is a required behavior):
1. Load `collab_projects` row (slug) and `project_packages` row; missing either ⇒ hard error (the receiver arm refreshes announcements first — see below). `manifest_xxh3` NULL ⇒ hard error `"announcement carries no manifest anchor"` (Д2, fail-closed).
2. Verify `xxh3_full_file(staging/manifest.ndjson) == manifest_xxh3` — mismatch ⇒ hard error naming both (this rejects a re-authored manifest before ANY record is parsed).
3. `read_manifest`; per record: `validate_rel_path`, payload present, `xxh3_full_file(payload) == record.xxh3`, `record.project.as_ref().map(|p| &p.project_id) == Some(project_id)` (stamp cross-check) — any failure ⇒ per-frame `Rejected(reason)` receipt, batch continues.
4. Landing root: `scan_root_path_of_kind(conn, "collaboration")` else `<staging parent's sync_dir>/collaboration` fallback (mirror the incoming-resolver fallback idiom); dest = `root/<sanitize_slug(project.slug)>/<sanitize_slug(package.publisher_display)>/<rel_path>` — publisher slug is HUB-anchored (Д5), never the serving peer, never the manifest.
5. uuid-supersede (Д9 receiver side): `replace_contribution_for_uuid(project_id, publisher, frame_uuid)` — when it returns an old landed path, delete that file best-effort (`warn!` on failure). If a contribution with the SAME `xxh3` already exists for `(project, publisher, uuid)` ⇒ receipt `Duplicate`, skip landing.
6. Land tmp-copy + atomic rename (mirror `land_payload`), `insert_contribution` (frame_meta/analysis JSON straight from the record), receipt `Ingested`, `insert_history_row` (Direction::Received, project dimension — Task 11 adds the column; until then object field carries `frame_meta.object` like personal sync).
7. On ≥1 Ingested: store the manifest bytes (`set_manifest`) and `set_local_status(package_id, "complete")` (partial failures still mark `complete` only when `failed.is_empty()`, else `failed`).

Receiver arm (in `receiver.rs`, replacing the Task-4 stub): authorized ProjectAnnounce ⇒ the ROW KEY is the event's hub `package_id` (audit B1) while fetch/ack use `announce.package_id` (wire id) ⇒ if the hub-id row is unknown, invoke a new `announcements_refresher: Option<Arc<dyn Fn(&str) + Send + Sync>>` hook (wired in Task 8 to the hub poll; absent or still-unknown afterwards ⇒ `warn!` + drop, fail-closed) ⇒ `validate_package_id` ⇒ staged `transport.fetch` (same as personal sync) ⇒ `spawn_blocking(ingest_project_package)` ⇒ `transport.ack(from, package_id, receipts)` ⇒ staging cleanup ⇒ `sync-progress`/`sync-finished` with `project_id: Some(..)` ⇒ post-ingest hook `on_project_ingested: Option<Arc<dyn Fn(String /*project*/, String /*package*/) + Send + Sync>>` (Task 8 wires report-have + notification data; absent = no-op).

- [ ] **Step 1:** failing unit tests for the pure parts (tempdir + in-memory conn seeded with project + package rows): manifest-anchor mismatch rejects everything; per-frame bad xxh3 ⇒ Rejected receipt while good frames land; landing path shape `<root>/<project-slug>/<publisher-slug>/<rel>`; uuid re-publication replaces the row AND the old file disappears; identical re-delivery ⇒ Duplicate receipts and no second file; stamp/project mismatch ⇒ Rejected.
- [ ] **Step 2:** implement `project_ingest.rs`; focused tests green.
- [ ] **Step 3:** receiver arm + hooks threading — the two optional hooks live on the Task-4 `ReceiverHooks` struct (threaded through `SyncRuntime::ensure_started` → `SyncReceiver::spawn`; api/sync.rs callers keep passing them as `None` until Task 8 wires them). Loopback e2e test (extend `sync` tests): node A enqueues a stamped package to node B (B's conn seeded with project + package row incl. correct manifest anchor) ⇒ B lands files under the collab layout, contributions rows exist, A's outbound row reaches `confirmed` (receipts flowed), `sync-finished` carries `project_id`.
- [ ] **Step 4:** gates + commit `feat(collab): project package ingest — anchored manifest, hub-resolved landing, contributions`.

### Task 6: Serve reconstruction + request-to-serve (ungated)

**Files:**
- Create: `crates/athenaeum-core/src/api/collab_exchange.rs` (UNGATED api module); Modify: `crates/athenaeum-core/src/api/mod.rs` (`pub mod collab_exchange;` — NO render gate)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (ProjectRequestReceived → handler hook on `ReceiverHooks`)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (wire the request handler at receiver start; `ensure_collab_sender_engine`)
- Modify: `crates/athenaeum-core/src/sync/engine.rs` — AUDIT M1: `spawn_with_sink` hard-codes `emitter: None` (`engine.rs:256-270`) and `spawn_inner` is private (`:276`); add `pub fn spawn_with_sink_and_emitter(store, transport, peer, sink, emitter)` delegating to `spawn_inner` so collab sends still emit `sync-progress`/`sync-finished`

**Interfaces:**
- Consumes: `SyncSenderRuntime`/`ensure_sender_engine` idiom (`api/sync.rs:621`), `PackageCleanupSink` (`engine.rs:188`), Task-4 `may_serve_package` + `ReceiverHooks`.
- Produces (BINDING):

```rust
// api/collab_exchange.rs
/// Rebuild a servable package dir for a locally-held package. origin='mine'
/// ⇒ returns the retained local_dir as-is. origin='received' ⇒ materializes
/// <sync_dir>/collab_serve/<package_id>/ (manifest bytes from the row,
/// payloads hard-linked from landed paths, copy fallback), returns it.
pub fn reconstruct_serve_dir(conn: &rusqlite::Connection, sync_dir: &std::path::Path, package_id: &str) -> anyhow::Result<std::path::PathBuf>;
/// Holder side of Д1: authorize + reconstruct + enqueue an explicit-target
/// send of the package to `from` through the collab sender map.
pub async fn handle_project_request(
    ctx: &crate::services::ServiceContext,
    sender: &crate::sync::SyncSenderRuntime,
    from: crate::sharing::types::NodeId,
    project_id: String,
    package_id: String,
    emitter: Option<std::sync::Arc<dyn crate::events::ProgressEmitter>>,
) -> anyhow::Result<()>;
pub struct CollabCleanupSink; // PackageCleanupSink: deletes dirs under collab_serve/, NEVER under collab_pub/
/// Mirror of api::sync::ensure_sender_engine (api/sync.rs:621) for the collab
/// map: same transport build shape but a DEDICATED `<sync_dir>/blobs_collab`
/// store dir (audit m7: a second FsStore over blobs_out risks the redb lock and
/// the startup tag-sweep, `api/sync.rs:647-654`, `iroh/mod.rs:373-383`), and
/// engines spawn via SyncEngine::spawn_with_sink_and_emitter(CollabCleanupSink,
/// emitter) so retained pub dirs survive confirm (Д4) while reconstructed serve
/// dirs are cleaned on terminal.
pub async fn ensure_collab_sender_engine(
    ctx: &crate::services::ServiceContext,
    sender: &crate::sync::SyncSenderRuntime,
    dest: crate::sharing::types::NodeId,
    emitter: Option<std::sync::Arc<dyn crate::events::ProgressEmitter>>,
) -> Result<(std::sync::Arc<crate::sync::SyncEngineHandle>, String), crate::api::ApiError>;
```

Rules in `handle_project_request`: package row must exist with `origin='mine' || local_status='complete'`; `may_serve_package(conn, project, state=="pending", &from)` must be true (pending ⇒ only the coordinator's nodes); violations ⇒ `warn!` + return Ok (silent drop, cross-account). The collab sender map is a SECOND `SyncSenderRuntime` instance (`AppState.collab_sender` on both hosts, Task 11) whose engines are spawned via `spawn_with_sink_and_emitter(CollabCleanupSink, emitter)` over the dedicated `blobs_collab` store — the retained publication dir must never be cleaned by a confirm (Д4), while reconstructed serve dirs are temp and are cleaned on terminal. (No dedup-responder work: project serves skip Offer/Want — Д2/Д10.)

- [ ] **Step 1:** failing tests: `reconstruct_serve_dir` (received package: dir contains byte-identical `manifest.ndjson` + hardlinked payloads at their `rel_path`s; second call idempotent; 'mine' returns `local_dir` untouched); `CollabCleanupSink` deletes `collab_serve/x` but refuses `collab_pub/x`.
- [ ] **Step 2:** implement; focused green.
- [ ] **Step 3:** `ReceiverHooks.project_request_handler: Option<Arc<dyn Fn(NodeId, String, String) + Send + Sync>>` invoked on `ProjectRequestReceived` (Task-4 stub replaced); the two `ensure_started` callers in `api/sync.rs` pass a closure that `tokio::spawn`s `handle_project_request` (ctx/sender clones). Loopback e2e: A holds a received-complete package; B sends `ProjectRequest`; A authorizes + serves; B lands it (Task-5 path) and A's collab outbound row confirms; a send-only node's request is silently refused; a pending package is served to the coordinator node only.
- [ ] **Step 4:** gates + commit `feat(collab): request-to-serve — holder authz, serve-dir reconstruction, dedup union`.

### Task 7: Publish — stamped build, hub announce, push-seed (render-gated)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` (publish section)
- Test: same file (api tests, `let (_tmp, ctx) = test_ctx();` + wiremock + loopback)

**Interfaces:**
- Consumes: `evaluate_project_gate` + `GateReport`/`FrameGateRow` (slice 3), `db::light_calibrations` (`output_path` per frame), `fits_writer::{stamp_extra_card, Card, CardValue}`, `package::{write_package, xxh3_full_file, MANIFEST_FILENAME}`, `ProjectStamp`, Task-2 `CollabClient::announce`, Task-3 db (`upsert_package`, `own_active_announcement_ids_for_uuids`, `mark_superseded`, `set_manifest`), Task-6 `ensure_collab_sender_engine` idiom + `AppState.collab_sender`, `collab::authz` (target selection reads the same members_json), hub creds via `api::account::hub_credentials`.
- Produces (BINDING for Tasks 11–12):

```rust
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    pub package_id: String,
    pub announcement_id: String,
    pub state: String,                    // pending | published
    pub frame_count: i64,
    pub byte_size: i64,
    pub superseded_announcements: Vec<String>,
    pub seed_target: Option<String>,      // display name of the chosen first-replica member, None = no eligible target
}
pub async fn publish_collab_frames(
    ctx: &ServiceContext,
    collab_sender: &crate::sync::SyncSenderRuntime,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<PublishResult, ApiError>;
```

Algorithm:
1. `evaluate_project_gate` → publishable rows; empty ⇒ `ApiError::Invalid("no publishable frames")`. Resolve each frame's `light_calibrations.output_path` (missing file ⇒ skip with `warn!`, collect into a `skipped` list; all skipped ⇒ Invalid).
2. Mint the HUB package uuid FIRST (`Uuid::new_v4()`) — it names the publication dir `<sync_dir>/collab_pub/<package-uuid>/payloads/…`, goes into every record's `ProjectStamp.package_id`, and is the `packageId` announced to the hub (audit B1; the writer-minted wire `PackageId` is ignored for identity). Per frame `stamp_extra_card(output_path, dest, &Card::new("ATH_PRJ", CardValue::Str(project_id.into()))?)`; `rel_path` = output filename, uniqued per package (mirror `unique_rel_path`, `api/sync.rs:713`).
3. `ManifestRecord` per frame: `frame_uuid = frames.uuid` (SOURCE identity, spec §3), `origin_catalog_uuid` = catalog uuid, `payload_kind: CalibratedLight`, `xxh3 = xxh3_full_file(stamped)`, `byte_size` of the stamped copy, `frame_meta` = Frame snapshot, `analysis` = frame_analysis JSON, `project: Some(ProjectStamp{ project_id, package_id: hub_package_uuid, thresholds_version: detail-thresholds version, cal_engine_version: light_calibrations.engine_version })`. `write_package` into the pub dir → `PackageAnnounce`.
4. Aggregate stats (from the gate rows of the PUBLISHED subset): `{"manifestXxh3": xxh3_full_file(pub_dir/manifest.ndjson), "fwhmArcsec": {"median","p10","p90"}, "eccentricityMedian", "frameCount", "integrationSecondsByFilter": {filter: Σ exptime}}` (filters/exptime from the frames rows; skip metrics that are absent — never invent).
5. Д9 supersedes: `own_active_announcement_ids_for_uuids(project_id, &published_uuids)`.
6. Hub `announce` (packageId = the pre-minted hub uuid from step 2; rootHash: the hub REQUIRES exactly 64 hex chars and `write_package`'s placeholder is 16-hex xxh3 — compute it as `iroh_blobs::Hash::new(&manifest_bytes)` rendered to 64-hex (BLAKE3; `blake3` is NOT a direct dep — audit M5 — but `iroh_blobs::Hash` is importable). Document on the field that the WIRE transfer substitutes the real iroh collection hash per serve, so this hub value is an identifier, not the fetch hash. Pre-dedupe `supersedes` and cap awareness: hub rejects >100 entries and duplicates; expect 409 `{"error"}` on a closed project or a globally-duplicate packageId — map via the slice-3 `client_err` idiom (audit m5)). On failure delete the pub dir (best-effort) and return the error.
7. Local rows: `upsert_package` (origin `'mine'`, own=1, state from the response, `local_dir` = pub dir, `manifest_xxh3`, `manifest_ndjson` bytes, `local_status='complete'`), `mark_superseded(&superseded)`; delete the superseded packages' retained dirs when origin='mine' (best-effort `warn!`).
8. Push-seed (Д8): parse the project's `members_json`; candidate nodes = coordinator's nodes when `state=="pending"`, else every `send_receive` member's nodes; exclude own node id (`DeviceKey` pubkey); pick the FIRST candidate; none ⇒ `seed_target: None` (UI shows "no receive-capable member online yet"). Else `ensure_collab_sender_engine(ctx, collab_sender, node, emitter)` + `enqueue_package(pub_dir)` — the engine's retry/backoff IS announce-and-wait; `seed_target = member display name`.

- [ ] **Step 1:** failing api tests: (a) e2e-ish publish over wiremock hub + loopback seed target: fixture project (members_json with a send_receive member whose node is a loopback peer), two gate-passing frames whose `output_path`s are real tiny FITS written by `write_fits_f32` in the fixture ⇒ publish returns state `published`, pub dir retained with stamped payloads (headers contain `ATH_PRJ`), package row origin=mine with manifest bytes, wiremock saw `aggregateStats.manifestXxh3` + correct `supersedes: []`; (b) re-publish of the same uuids ⇒ wiremock body carries the first announcement id in `supersedes` and the first package row flips `superseded=1`; (c) `require_approval` project ⇒ hub answers `pending` and the seed target is the coordinator's node, not the other member's; (d) no eligible target ⇒ `seed_target: None`, no enqueue, still announced.
- [ ] **Step 2:** implement per the algorithm; focused green; full core suite green.
- [ ] **Step 3:** gates (incl. perseus no-default-features — publish is render-gated so headless must still compile) + commit `feat(collab): publish — stamped package, anchored announce, supersedes, push-seed`.

### Task 8: Announcements poll + download orchestration (split gating)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (poll + download — UNGATED: no gate/lights dependency)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (wire `announcements_refresher` + `on_project_ingested` hooks from Task 5)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` ONLY if a transport handle getter is missing (see step 3)

**Interfaces:**
- Consumes: Task-2 client (`list_announcements`, `report_have`), Task-3 db, Task-5 hooks, `hub_credentials`, `SyncRuntime` (receiver's transport for outbound `request_project`), holders wire (`HolderWire.pubkey` base64 32-byte → NodeId via `pairing::node_id_from_pubkey_b64`).
- Produces (BINDING):

```rust
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackageStateChange { pub project_id: String, pub package_id: String, pub kind: String, pub detail: Option<String> }
// kind ∈ "newPackage" | "approved" | "rejected" | "downloadComplete" | "downloadFailed"
/// Poll one project's announcements into project_packages. Returns the diffs
/// the frontend turns into notify() calls. NEVER notifies itself.
pub async fn refresh_project_packages(ctx: &ServiceContext, project_id: &str) -> Result<Vec<PackageStateChange>, ApiError>;
/// All projects (poll cadence entry point).
pub async fn refresh_all_project_packages(ctx: &ServiceContext) -> Result<Vec<PackageStateChange>, ApiError>;
/// Д6 explicit download: sequential holder loop, spawned; progress via the
/// existing sync events; terminal state lands in project_packages.local_status.
pub async fn download_project_package(ctx: &ServiceContext, sync: &crate::sync::SyncRuntime, project_id: &str, package_id: &str) -> Result<(), ApiError>;
```

Poll rules: upsert every listed announcement (origin `'remote'` unless `own` ⇒ `'mine'`-preserving, never downgrade an existing `'received'`/`'mine'`); `manifest_xxh3` extracted from `aggregateStats.manifestXxh3` (string); state transitions produce `PackageStateChange`s: unknown→any published = `newPackage` (skip own), pending→published on an OWN row = `approved`, →rejected = `rejected` with `detail = reject_reason`; hub-listed `supersedes` ⇒ `mark_superseded`; `holder_count = holders.len()`, `online_count` = holders with `lastSeenAt` within 5 minutes (hub online semantics), both written on every poll. Packages the hub no longer lists are left in place (history).
Download rules: role guard — the caller must be send_receive/coordinator (read own role from members_json via account… the app's own account id is not in the snapshot keying; determine own membership by matching OWN node id via `authz::member_for_node` — fail-closed Invalid otherwise); `set_local_status("downloading")`; holders from the freshly-polled row (exclude own node); sequentially per holder: `transport.request_project(holder_node, project, package)` then poll the row every 2s up to 90s for `local_status == "complete"` (the Task-5 receiver arm flips it when the served package ingests); on success `report_have` (device bearer) + return; all holders exhausted ⇒ `set_local_status("failed")` + `PackageStateChange{kind:"downloadFailed"}` recorded via the on-ingest hook path… (no: download runs in a command-spawned task — it RETURNS after spawning; the terminal notification rides the next poll's diff of local_status, plus the `sync-finished` event already fires live). Keep it exactly this simple — v1.
Hook wiring (in `api/sync.rs` at receiver start): `announcements_refresher = |project_id| tokio::spawn(refresh_project_packages(...))` (block_on-free: the receiver arm re-checks after awaiting the refresher via a small oneshot — implement as an async closure the receiver awaits, matching however Task 5 defined the hook signature); `on_project_ingested = |project, package| tokio::spawn(async { report_have best-effort (warn! on failure) })`.

- [ ] **Step 1:** failing tests: poll upsert + diff kinds (wiremock list fixtures: new published foreign row ⇒ `newPackage`; own pending→published across two polls ⇒ `approved`; rejected with reason ⇒ `rejected` + detail; supersedes marking); download role guard (send-only own node ⇒ Invalid); download happy path over loopback (holder A completes the Task-6 e2e circuit; the poll-loop observes `complete`; `report_have` hits wiremock).
- [ ] **Step 2:** implement; focused green.
- [ ] **Step 3 (AUDIT B4 — dial hints are mandatory):** the receiver runtime holds only `Arc<dyn SharingTransport>` (`receiver.rs:364-368`), and a bare node id CANNOT be dialed — `dial_target` falls back to a hint-less `EndpointAddr` (`iroh/mod.rs:281-287`) and fails with "No addressing information available"; the sender path only works because it attaches `pairing::peer_addr_with_relays` via the inherent `IrohTransport::add_peer` (`api/sync.rs:666-674`, `iroh/mod.rs:251`). Therefore: (a) `SyncRuntime` stores an additional `Arc<IrohTransport>` (concrete) alongside the trait object when it builds one, exposed as `pub fn iroh_handle(&self) -> Option<Arc<IrohTransport>>`; (b) the download loop, per holder, resolves relay urls exactly like `ensure_sender_engine` does and calls `handle.add_peer(pairing::peer_addr_with_relays(holder_node, &relay_urls))` BEFORE `request_project`. Loopback tests bypass dialing (mailbox routing) — add a doc-comment on `download_project_package` stating the hint step is what real-network paths depend on.
- [ ] **Step 4:** gates + commit `feat(collab): announcements poll with state diffs + sequential-holder download`.

### Task 9: Moderation — review data + approve/reject (render-gated)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs`

**Interfaces:**
- Consumes: Task-2 client (`approve_announcement`, `reject_announcement`), Task-3 db (`get_package_by_announcement`, `contributions_for_package`, `delete_package`), Task-8 poll (pending rows arrive via it), slice-3 `get_project_detail` idioms.
- Produces (BINDING):

```rust
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModerationFrame { pub frame_uuid: String, pub rel_path: String, pub landed_path: Option<String>, pub byte_size: i64, pub fwhm: Option<f64>, pub eccentricity: Option<f64>, pub stars: Option<i64>, pub snr: Option<f64> }
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModerationItem { pub announcement_id: String, pub package_id: String, pub publisher: String, pub frame_count: i64, pub byte_size: i64, pub created_at: String, pub review_copy_complete: bool, pub frames: Vec<ModerationFrame> }
pub fn list_moderation_queue(ctx: &ServiceContext, project_id: &str) -> Result<Vec<ModerationItem>, ApiError>;
pub async fn decide_announcement(ctx: &ServiceContext, announcement_id: &str, approve: bool, reason: Option<String>) -> Result<(), ApiError>;
```

`list_moderation_queue`: pending `project_packages` rows for the project; `frames` from `contributions_for_package` (metrics parsed from the stored `analysis` JSON — absent metric ⇒ None, never invented); `review_copy_complete = local_status == "complete"` (the coordinator may still be receiving the push-seed). `decide_announcement`: approve ⇒ hub approve + local state `published`. Reject ⇒ `reason` required non-empty (hub demands 1..=500) ⇒ hub reject ⇒ delete the review copy: every contribution's landed file best-effort removed (`warn!` per failure), then `delete_package` (CASCADE clears rows) — the spec's "the coordinator's local review copy is removed". Hub 409 (already decided) surfaces as Conflict via the `client_err` idiom; local state is then re-synced by the next poll (do NOT mutate locally on 409).

- [ ] **Step 1:** failing tests: queue lists pending-only with metrics parsed from analysis JSON and `review_copy_complete` both ways; approve flips local state (wiremock 200); reject with empty reason ⇒ Invalid before any hub call; reject happy path deletes landed files + rows (fixture lands two tiny files first); hub 409 ⇒ Conflict and the local row is untouched.
- [ ] **Step 2:** implement; focused green.
- [ ] **Step 3:** gates + commit `feat(collab): in-app moderation — review queue, approve/reject with review-copy cleanup`.

### Task 10: Scanner `ATH_PRJ` reconcile branch (ungated)

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/calibrated_light.rs` (identity gains the project card)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (sibling branch BEFORE the light-cal reconcile)

**Interfaces:**
- Consumes: `CalibratedIdentity` (`calibrated_light.rs:26`), `calibrated_light_identity(keys)` (`:81`), the reconcile hook sites (`scanner/mod.rs:454-470` sequential, `:1376-1382` + `:1734-1759` parallel), Task-3 db (`contributions` lookups by landed_path/xxh3).
- Produces: `CalibratedIdentity.project_id: Option<String>` (from `ATH_PRJ`); `fn reconcile_project_contribution(conn, path, current_path, identity, ...) -> anyhow::Result<()>` in `scanner/mod.rs`.

Branch semantics (sibling of the light-cal four-branch, spec §7 — a stamped file NEVER enters `frames`):
- Identity carries `ATH_PRJ` ⇒ route here INSTEAD of `reconcile_calibrated_light` (check project first — a project file also carries `ATH_CSRC`).
- **known**: a contribution row's `landed_path == current_path` ⇒ debug no-op.
- **moved**: a row matches `(project_id, xxh3 of file)` and its `landed_path` no longer exists on disk ⇒ UPDATE landed_path, `info!`.
- **duplicate**: row matches by xxh3 and old path still exists ⇒ `warn!(kept, duplicate, "duplicate project contribution copy")`, untouched.
- **unknown**: no row (hand-dropped stamped file, or the project/package is gone) ⇒ `warn!(path, project_id, "project contribution without a tracking row — leaving untouched (re-download via the project page)")` and defer — NEVER auto-insert (a contribution row requires hub-anchored package context we don't have here; idempotent on re-scan).
Both scan paths (sequential + parallel) must divert; the file contributes nothing to `files`/`frames`/clustering/duplicates.

- [ ] **Step 1:** failing tests: identity parses `ATH_PRJ`; scanner unit test seeding a contribution row + a stamped file at its landed_path ⇒ scan is a no-op (no `files` row); moved case repoints; unknown stamped file ⇒ warn + no rows anywhere (assert `files` empty).
- [ ] **Step 2:** implement both paths; focused + full core suite green.
- [ ] **Step 3:** gates + commit `feat(collab): scanner ATH_PRJ reconcile — project contributions never enter the catalog`.

### Task 11: Commands (both backends), ts_export, events + Transfers project dimension

**Files:**
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`DDL_HISTORY` + `HISTORY_COLS` + insert/search: `project TEXT` column), `crates/athenaeum-core/src/sync/models.rs` (`HistoryRow.project: Option<String>`, `HistoryQuery.project: Option<String>`), `crates/athenaeum-core/src/sync/engine.rs` + `ingest.rs` + `project_ingest.rs` (populate it)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register: `PublishResult`, `PackageStateChange`, `ProjectPackageView`, `ContributionView`, `ModerationFrame`, `ModerationItem`, updated `SyncProgressEvent`/`SyncFinishedEvent`/`HistoryRow`/`SyncHistoryQuery`)
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs` + `commands/mod.rs` + `lib.rs` `invoke_handler![]`; `crates/athenaeum-tauri/src/commands/mod.rs` `AppState` gains `collab_sender: Arc<SyncSenderRuntime>`
- Modify: `crates/athenaeum-web/src/routes/collab.rs` + `routes/mod.rs`; `WebAppState` gains the same
- Create in core: `ProjectPackageView`/`ContributionView` list DTOs in `api/collab_exchange.rs`. `list_project_packages(ctx, project_id)` reads `project_packages` rows only — no live hub call; `ProjectPackageView` carries `holder_count`/`online_count` (the Task-3 columns, written at poll time by Task 8) plus `package_id/state/local_status/own/publisher/byte_size/frame_count/created_at/reject_reason/superseded`.

**Command surface (7, each Tauri + Axum mirror in the same commit; wrappers 3–7 lines; `#[tracing::instrument(skip_all, err)]` / `err(Debug)`):**

| command | args | returns | core fn |
| ---- | ---- | ---- | ---- |
| `publish_collab_package` | `projectId` | `PublishResult` | `api::collab::publish_collab_frames` (Tauri passes `state.collab_sender`; web its own) |
| `refresh_collab_packages` | — | `Vec<PackageStateChange>` | `api::collab_exchange::refresh_all_project_packages` |
| `list_collab_packages` | `projectId` | `Vec<ProjectPackageView>` | `api::collab_exchange::list_project_packages` |
| `download_collab_package` | `projectId, packageId` | `()` (spawns) | `api::collab_exchange::download_project_package` (needs `state.sync`) |
| `list_collab_contributions` | `projectId` | `Vec<ContributionView>` | `api::collab_exchange::list_contributions` |
| `list_collab_moderation` | `projectId` | `Vec<ModerationItem>` | `api::collab::list_moderation_queue` |
| `decide_collab_announcement` | `announcementId, approve, reason?` | `()` | `api::collab::decide_announcement` |

`publish/list_collab_moderation/decide` wrappers are render-path-only in core but BOTH host crates always build with render — no extra gating in the hosts (mirror slice-3's arrangement). History project dimension: `project` column populated by the engine (`ManifestRecord.project` stamp at `append_*_history`), by `project_ingest` rows, NULL for personal sync; `TransfersPanel` query passthrough.

- [ ] **Step 1:** history column + row/query structs + producers. AUDIT B3 — `CREATE TABLE IF NOT EXISTS` never adds a column to an EXISTING table: add a guarded migration (`column_exists` + `ALTER TABLE sync_history ADD COLUMN project TEXT` — idiom at `db/schema.rs:49` used e.g. `:814`, `:1304`) at ALL THREE materialization sites: `db/schema.rs:1653` region, `StandaloneSyncStore::open` (`sync/store.rs:544` — use/extend Perseus's `ensure_column` idiom, `crates/perseus/src/seen.rs:51-65`), `CatalogSyncStore::open` (`store.rs:728`). AUDIT m2 — `HistoryRow` literals that must gain `project: None`: production `crates/perseus/src/run.rs:1160`, `:1181`; tests `perseus/src/web.rs:1592,1605`. Core tests for insert/search with project filter + a migration test (open a store over a pre-existing project-less `sync_history`, insert succeeds).
- [ ] **Step 2:** list DTOs + `list_project_packages`/`list_contributions` with tests (poll captures holder counts into the columns).
- [ ] **Step 3:** 7 commands + mirrors + registrations + `AppState`/`WebAppState.collab_sender` (constructed where `sync_sender` is); ts_export decls + `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- [ ] **Step 4:** all slice gates (`cargo build --workspace`, core suite, perseus suite + no-default-features build, `npx tsc --noEmit`) + commit `feat(collab): exchange command surface both backends, history project dimension, ts types`.

### Task 12: Frontend — Contribute publish, Receive, Moderation, Transfers chip

**Files:**
- Modify: `src/pages/ProjectDetail.tsx` (publish flow + Receive tab + Moderation tab + storage banner)
- Create: `src/components/collab/ModerationQueue.tsx`, `src/components/collab/ReceiveTab.tsx` (keep `ProjectDetail.tsx` from bloating — tabs render these)
- Modify: `src/hooks/useProjects.ts` (poll also calls `refresh_collab_packages` and maps `PackageStateChange[]` → `notify()`)
- Modify: `src/components/transfers/TransfersPanel.tsx` (project chip + filter passthrough)

**Interfaces:** generated types from Task 11; commands per its table; `notify()` kind `project`; design tokens only; `api.listen` cancelled-flag if any listener is added (prefer NONE new — transfers events are already consumed globally).

Behaviors (each an acceptance item):
1. **Contribute tab**: "Publish N passing frames" button (enabled when `gate.publishable > 0`) → confirm dialog stating frame count + byte estimate + (approval projects) "goes to <coordinator> for review" → `publish_collab_package` → success notify (`kind:'project'`, dedupe `publish-<packageId>`) — message per state: published → "Publication announced — seeding to <seedTarget>" / pending → "Sent for approval to <seedTarget>" / no target → "Announced — waiting for a receive-capable member". **Publication history** section under it: own packages from `list_collab_packages` (`own`), each row: created, frames/bytes, state chip (pending amber / published green / rejected red with reason on title), replication line "held by K (N online)", superseded rows dimmed.
2. **Receive tab** (only when own role is send_receive/coordinator — derive from `detail.members` + own display?? NO: derive from `list_collab_packages` succeeding is wrong too — slice-3 `ProjectDetail.card.dataRole` carries the own role; use it): package list (non-own or all-with-own-marked), state: available (Download button; disabled with title when `holderCount == 0` — "no online holders") / downloading (spinner via `local_status`) / complete (landed under the collaboration folder — path hint) / failed (Retry). Empty collaboration root ⇒ banner "Set a Collaboration folder first" linking `/files` (the SpecialFolderSection lives there) and Download disabled.
3. **Moderation tab**: visible iff `card.coordinator && card.requireApproval`; badge = pending count; per item: publisher, frames/bytes, `review_copy_complete ? metrics table (fwhm/ecc/stars/snr per frame) : "receiving review copy…"`; Approve button; Reject button opens a reason dialog (required, ≤500) — both call `decide_collab_announcement`, refresh on success, errors surfaced inline (no silent catch).
4. **useProjects poll**: after `refresh_collab_projects`, call `refresh_collab_packages`; map changes: `newPackage` → "New package available in <project>" (dedupe `pkg-new-<packageId>`), `approved` → "Your contribution was approved", `rejected` → "Your contribution was rejected: <detail>" (tone warning), `downloadFailed` → warning. All `kind:'project'`, `link:'/projects/<projectId>'`.
5. **TransfersPanel**: history rows show a `project` chip when the new field is set; the free-text filter also matches it.

- [ ] **Step 1:** implement per behavior list; `npx tsc --noEmit` clean.
- [ ] **Step 2:** self-review against the security section below; commit `feat(ui): collab exchange — publish, receive, moderation, transfers project dimension`.

## Security requirements (bind Tasks 4–9, 12)

- **S1 wire trust:** every peer-supplied string that touches a path goes through `validate_package_id`/`validate_rel_path`; landing prefixes are hub-anchored (Д5); the manifest is anchor-verified before parsing records (Д2); a stamp/project mismatch rejects the frame. No exceptions, including "our own" packages.
- **S2 fail-closed authz:** empty/missing snapshot rows authorize nobody; the connect gate refuses unknown nodes; pending packages serve only to coordinator nodes; send-only members are never served; violations are silent drops + `warn!` (no error responses that enumerate membership).
- **S3 no secrets in logs/UI:** tokens never logged; node ids logged as hex (existing idiom); reject reasons and hub strings render as text (React escapes; no dangerouslySetInnerHTML).
- **S4 no unbounded resource use:** download loop bounded (90s/holder, holders list finite); reconstruction hardlinks (no byte copies unless cross-device); publish skips-with-warn rather than failing the batch on one bad file.
- **S5 catalog isolation:** contributions never create `files`/`frames` rows (Task 5 inserts only `project_contributions`; Task 10 guarantees the scanner path; assert in tests).
- **S6 UI honesty:** every state chip derives from stored rows/hub state, never optimistic; failed hub calls surface (notify or inline error), never silently caught.

## Post-plan checklist (verification/ops notes, not tasks)

- Live smoke needs TWO app instances (desktop dev + local web on a scratch DB copy — the slice-3 smoke pattern) against test-hub: publish from one, download on the other, moderation with `require_approval` on.
- The connect gate (Д7) changes receiver behavior for personal sync too (account devices still pass via the composite) — watch the observatory Perseus → desktop path on the next live sync after merge.
- Deferred (recorded): parallel multi-source fetch (spec §12); auto-fetch toggle (Д6); WBPP project export + E2E harness = slice 5; intent/archived-set checks from the slice-3 deferred list.

## Self-review notes (applied while writing)

- The hub `rootHash` 64-hex requirement vs the 16-hex placeholder was caught and resolved (BLAKE3 of manifest bytes, Task 7 step 6) — the wire fetch hash is per-serve anyway (engine substitutes the served collection hash; slice-1 hub validates format only).
- `SyncEngineHandle`/receiver hook additions keep both existing spawn call sites compiling by passing `None` (Tasks 5/6 update them in the same commits).
- Task-3 DDL includes `holder_count`/`online_count` used by Task 11 — single DDL owner.
- Perseus: every touched ungated module must keep `cargo test -p perseus` + no-default-features build green (Msg/manifest/history-column changes compile into Perseus; its code passes `project: None` / ignores new variants).
- `sanitize_slug`/`unique_path` visibility may need `pub(crate)` adjustments (Task 5) — same-module reuse, no API leak.
