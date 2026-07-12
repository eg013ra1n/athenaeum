# Sync Dedup Handshake (Plan 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Before the existing `Announce`, run a best-effort P2P `Offer → Want → FullHashes` handshake so each receiver fetches only the frames it is missing.

**Architecture:** A new **request/response** control exchange on the existing `athenaeum/sync/1` ALPN (the transport is one-way/event-driven today, so we add one `negotiate_want` bi-stream method whose accept-side answer is computed by a catalog-backed **dedup responder** injected into the control handler, exactly like `PeerAuthorizer`). The pure want/candidate split lives in a testable `sync/dedup.rs`. The sender computes the sampling hash from the package-dir payloads (uniform for app + Perseus), gets the full `xxh3` for free from the manifest, negotiates, then serves a **want-subset collection** (Task 7 build-once is preserved; the cleanup coordinator is unchanged). Any handshake error → fall back to announcing the full package.

**Tech Stack:** Rust (`athenaeum-core` only), iroh + iroh-blobs (QUIC), postcard wire format, xxHash (`duplicates::compute_xxhash` sampling, `package::xxh3_full_file` full).

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-07-12-sync-dedup-handshake-design.md`. Refines §7 of `2026-07-11-sync-model-phase1-design.md`.
- **Best-effort:** correctness NEVER depends on the handshake. Any error/timeout/old-peer → the sender announces the FULL package (current behavior). The receiver's ingest-time uuid/content dedup is the safety net.
- **Hash distinction (load-bearing):** `files.content_hash` = the **sampling** hash (`duplicates::compute_xxhash`, first/middle/last 512 KB). `ManifestRecord.xxh3` / `FrameReceipt.xxh3` = the **full** hash (`package::xxh3_full_file`). `Offer.sampling_hash` keys on the former; a sampling match is a *candidate* only — never skipped without a **full-hash** confirm (the sampling hash can false-positive; a blind skip would silently drop a genuinely-new file).
- **Hub-free:** no hub involvement, no DB-schema change, no new dependency.
- **Path safety unchanged:** `rel_path` stays `validate_rel_path`-guarded on both write and receive; the offer/want carry `rel_path` but nothing is written from them.
- **Serde/postcard:** new `Msg` variants encode/decode via the existing postcard path; `TransportEvent` additions are in-process only (no serde).
- **Author every commit as `eg013ra1n <vilen.sharifov@gmail.com>`; no Claude co-author/footer.** GitLab `origin` only. Branch `0.4.0`.
- **Gates per task:** `cargo test -p athenaeum-core` (+ `--test sync_e2e` where noted) green and warning-free; `cargo build --workspace` clean. Perseus + app both consume the engine change but need no edits.

---

### Task 1: Wire protocol — `Offer`/`Want`/`FullHashes` variants

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/proto.rs` (add 3 `Msg` variants + 2 entry structs)
- Test: `crates/athenaeum-core/src/sharing/iroh/proto.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces (consumed by Tasks 2–6):
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct OfferEntry { pub rel_path: String, pub sampling_hash: String, pub byte_size: u64 }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct FullHashEntry { pub rel_path: String, pub sampling_hash: String, pub xxh3_full: String }
  // added to enum Msg:
  //   Offer { package_id: PackageId, entries: Vec<OfferEntry> }
  //   Want  { package_id: PackageId, want: Vec<String>, candidates: Vec<String> }   // keyed by rel_path
  //   FullHashes { package_id: PackageId, entries: Vec<FullHashEntry> }
  ```

- [ ] **Step 1: Write the failing test**

```rust
// in proto.rs tests module
#[test]
fn offer_want_fullhashes_roundtrip() {
    let pid = PackageId("pkg-1".into());
    for msg in [
        Msg::Offer { package_id: pid.clone(), entries: vec![
            OfferEntry { rel_path: "M31/L_0001.fits".into(), sampling_hash: "00ff".into(), byte_size: 12 } ] },
        Msg::Want { package_id: pid.clone(), want: vec!["a".into()], candidates: vec!["b".into()] },
        Msg::FullHashes { package_id: pid.clone(), entries: vec![
            FullHashEntry { rel_path: "b".into(), sampling_hash: "00ff".into(), xxh3_full: "1122334455667788".into() } ] },
    ] {
        let back = Msg::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(msg, back);
    }
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib offer_want_fullhashes_roundtrip` → FAIL (variants/structs absent).

- [ ] **Step 3: Implement** — add `OfferEntry`/`FullHashEntry` structs and the three variants to `enum Msg` (keep `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`). Update the module doc (currently "Two message kinds") to reflect the handshake. No `encode`/`decode` change needed (postcard handles the new variants).

- [ ] **Step 4: Run** — the new test + `cargo test -p athenaeum-core --lib` (proto module) green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/iroh/proto.rs
git commit -m "feat(sync): Offer/Want/FullHashes control messages for the dedup handshake"
```

---

### Task 2: Pure dedup split logic — `sync/dedup.rs`

The want/candidate partition and the full-hash confirm, as pure functions (no transport, no DB), so they are unit-tested directly.

**Files:**
- Create: `crates/athenaeum-core/src/sync/dedup.rs`
- Modify: `crates/athenaeum-core/src/sync/mod.rs` (`mod dedup;` + re-export)
- Test: `crates/athenaeum-core/src/sync/dedup.rs` (inline)

**Interfaces:**
- Consumes: `OfferEntry`, `FullHashEntry` (Task 1).
- Produces:
  ```rust
  use std::collections::{HashMap, HashSet};
  use crate::sharing::iroh::proto::{OfferEntry, FullHashEntry};

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct WantSplit { pub want: Vec<String>, pub candidates: Vec<String> } // rel_paths

  /// want = offered entries whose sampling_hash is NOT present locally (definitely absent);
  /// candidates = sampling_hash IS present (possible duplicate, must full-confirm).
  pub fn partition_offer(entries: &[OfferEntry], local_sampling_present: &HashSet<String>) -> WantSplit;

  /// For candidates: a candidate is a TRUE duplicate iff its full xxh3 is among the full
  /// hashes of the receiver's local files that share its sampling hash. Returns the rel_paths
  /// that are STILL wanted (sampling false-positives). `local_full_by_sampling` maps a
  /// sampling_hash → the set of full xxh3 of local files carrying that sampling hash.
  pub fn confirm_candidates(entries: &[FullHashEntry],
      local_full_by_sampling: &HashMap<String, HashSet<String>>) -> Vec<String>;
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn oe(rel: &str, s: &str) -> OfferEntry { OfferEntry { rel_path: rel.into(), sampling_hash: s.into(), byte_size: 1 } }
    fn fe(rel: &str, s: &str, f: &str) -> FullHashEntry { FullHashEntry { rel_path: rel.into(), sampling_hash: s.into(), xxh3_full: f.into() } }

    #[test]
    fn absent_sampling_is_want_present_is_candidate() {
        let offered = [oe("new.fits", "aaaa"), oe("maybe.fits", "bbbb")];
        let local: HashSet<String> = ["bbbb".to_string()].into_iter().collect();
        let split = partition_offer(&offered, &local);
        assert_eq!(split.want, vec!["new.fits".to_string()]);
        assert_eq!(split.candidates, vec!["maybe.fits".to_string()]);
    }

    #[test]
    fn true_dup_dropped_false_positive_wanted() {
        // candidate "dup" full-matches a local full hash → dropped; "collide" samples-match
        // but full differs → still wanted.
        let cands = [fe("dup.fits", "bbbb", "1111111111111111"), fe("collide.fits", "bbbb", "2222222222222222")];
        let mut local_full: HashMap<String, HashSet<String>> = HashMap::new();
        local_full.insert("bbbb".into(), ["1111111111111111".to_string()].into_iter().collect());
        let still = confirm_candidates(&cands, &local_full);
        assert_eq!(still, vec!["collide.fits".to_string()]); // dup dropped, collide kept
    }
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib dedup::` → FAIL (module absent).

- [ ] **Step 3: Implement**

```rust
//! Pure want/candidate split for the pre-transfer dedup handshake (spec §7).
//! No transport, no DB — the catalog lookups live in the responder (api/db);
//! this is the decision logic, unit-tested in isolation.
use std::collections::{HashMap, HashSet};
use crate::sharing::iroh::proto::{OfferEntry, FullHashEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantSplit { pub want: Vec<String>, pub candidates: Vec<String> }

pub fn partition_offer(entries: &[OfferEntry], local_sampling_present: &HashSet<String>) -> WantSplit {
    let mut want = Vec::new();
    let mut candidates = Vec::new();
    for e in entries {
        if local_sampling_present.contains(&e.sampling_hash) { candidates.push(e.rel_path.clone()); }
        else { want.push(e.rel_path.clone()); }
    }
    WantSplit { want, candidates }
}

pub fn confirm_candidates(entries: &[FullHashEntry],
    local_full_by_sampling: &HashMap<String, HashSet<String>>) -> Vec<String> {
    entries.iter().filter(|e| {
        // true duplicate iff a local file with the same sampling hash also has the same FULL hash
        !local_full_by_sampling.get(&e.sampling_hash).is_some_and(|set| set.contains(&e.xxh3_full))
    }).map(|e| e.rel_path.clone()).collect()
}
```
Add `mod dedup;` to `sync/mod.rs` and `pub use dedup::{partition_offer, confirm_candidates, WantSplit};`.

- [ ] **Step 4: Run** — the two tests + `cargo test -p athenaeum-core` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/dedup.rs crates/athenaeum-core/src/sync/mod.rs
git commit -m "feat(sync): pure want/candidate dedup split (partition_offer + confirm_candidates)"
```

---

### Task 3: Catalog lookup + dedup responder

The DB membership helper (`sampling_hash IN (…) → present`), the full-hash re-hash of local candidates, and the `DedupResponder` the transport calls on the receive side.

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs` (`find_files_by_content_hashes`)
- Modify: `crates/athenaeum-core/src/sync/store.rs` (`CatalogSyncStore::files_by_sampling_hashes` wrapper)
- Create: `crates/athenaeum-core/src/sync/responder.rs` (`DedupResponder` trait + `CatalogDedupResponder`)
- Modify: `crates/athenaeum-core/src/sync/mod.rs` (`mod responder;` + re-export)
- Test: `crates/athenaeum-core/src/sync/responder.rs` (inline, with an in-memory catalog)

**Interfaces:**
- Consumes: `partition_offer`/`confirm_candidates` (Task 2); `OfferEntry`/`FullHashEntry` (Task 1); `package::xxh3_full_file` (`package/mod.rs:130`); `CatalogSyncStore::lock_conn` (`sync/store.rs:742`).
- Produces:
  ```rust
  // db/operations.rs — content_hash + path are both indexed (idx_files_content_hash, idx_files_path)
  pub fn find_files_by_content_hashes(conn: &rusqlite::Connection, hashes: &[String])
      -> rusqlite::Result<Vec<(String /*content_hash*/, String /*path*/)>>;
  // sync/store.rs
  impl CatalogSyncStore { pub fn files_by_sampling_hashes(&self, hashes: &[String]) -> anyhow::Result<Vec<(String,String)>>; }
  // sync/responder.rs
  pub trait DedupResponder: Send + Sync {
      fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>); // (want, candidates) rel_paths
      fn confirm_full_hashes(&self, entries: &[FullHashEntry]) -> Vec<String>;        // still-wanted rel_paths
  }
  pub struct CatalogDedupResponder { store: std::sync::Arc<crate::sync::store::CatalogSyncStore> }
  impl CatalogDedupResponder { pub fn new(store: std::sync::Arc<crate::sync::store::CatalogSyncStore>) -> Self; }
  ```

- [ ] **Step 1: Write the failing test**

```rust
// responder.rs tests — build a CatalogSyncStore over an in-memory/tempfile DB with a couple of files.
#[tokio::test]
async fn responder_wants_absent_and_confirms_candidates() {
    let store = test_store_with_files(&[
        // (path, sampling_hash=content_hash, full_xxh3 of the on-disk file)
    ]); // helper: insert files rows + write real payload files whose xxh3_full_file matches
    let r = CatalogDedupResponder::new(store);
    let (want, cands) = r.want_for_offer(&[
        OfferEntry { rel_path: "new.fits".into(), sampling_hash: "absent".into(), byte_size: 1 },
        OfferEntry { rel_path: "have.fits".into(), sampling_hash: "<the local sampling hash>".into(), byte_size: 1 },
    ]);
    assert_eq!(want, vec!["new.fits"]);
    assert_eq!(cands, vec!["have.fits"]);
    // full-hash confirm: sending the SAME full hash as the local file → dropped (empty still-wanted)
    let still = r.confirm_full_hashes(&[FullHashEntry {
        rel_path: "have.fits".into(), sampling_hash: "<the local sampling hash>".into(), xxh3_full: "<local full>".into() }]);
    assert!(still.is_empty());
}
```
(Follow the existing `sync/store.rs` / `sync/receiver.rs` test harness for building a `CatalogSyncStore` over a temp DB; the helper writes real files so `xxh3_full_file` returns a known value. If the store test harness is awkward, mirror `sync/ingest_tests.rs`'s `fixture_*` builders.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib responder_wants_absent_and_confirms_candidates` → FAIL.

- [ ] **Step 3: Implement**
  - `find_files_by_content_hashes`: build `WHERE content_hash IN (?,?,…)` from `hashes` (chunk if >999 to respect SQLite's bind limit), `SELECT content_hash, path`. Return `(content_hash, path)` rows.
  - `files_by_sampling_hashes`: `self.lock_conn()` then delegate; `.map_err(anyhow)`.
  - `CatalogDedupResponder::want_for_offer`: collect the offered `sampling_hash`es, query `files_by_sampling_hashes`, build the present-set, call `partition_offer`.
  - `CatalogDedupResponder::confirm_full_hashes`: collect the entries' `sampling_hash`es, query `files_by_sampling_hashes` → for each `(sampling, path)` compute `package::xxh3_full_file(path)` (skip a path that's missing on disk — a stale row), build `HashMap<sampling, HashSet<full>>`, call `confirm_candidates`. Log a `tracing::debug!` with counts; never panic on a missing/unhashable file (treat as no-local-match → keep the candidate wanted, the safe direction).
  - `mod responder;` + `pub use responder::{DedupResponder, CatalogDedupResponder};` in `sync/mod.rs`.

- [ ] **Step 4: Run** — the test + `cargo test -p athenaeum-core` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/operations.rs crates/athenaeum-core/src/sync/store.rs crates/athenaeum-core/src/sync/responder.rs crates/athenaeum-core/src/sync/mod.rs
git commit -m "feat(sync): catalog dedup responder — sampling-hash membership + full-hash candidate confirm"
```

---

### Task 4: Transport handshake — `negotiate_want` + responder-answered accept branch + loopback

Add the request/response primitive. The sender calls `negotiate_want` (opens a bi-stream, sends `Offer`, reads a `Want` reply; if candidates, sends `FullHashes`, reads the final `Want`). The receiver's `SyncControlProtocol` answers `Offer`/`FullHashes` via an injected `DedupResponder` (writing a real `Want` Msg back instead of the one-byte delivery ack). Best-effort: any error → `Err` (the engine falls back).

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (`SharingTransport::negotiate_want` method, default `Err("unsupported")`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` (`negotiate_want` impl; `SyncControlProtocol` gains `Option<Arc<dyn DedupResponder>>`; accept-branch answers Offer/FullHashes; a `send_request` that reads a `Msg` reply)
- Modify: `crates/athenaeum-core/src/sharing/loopback.rs` (`negotiate_want` via the target peer's responder; `PeerInbox.responder`)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` (thread the `Arc<dyn DedupResponder>` into the transport when the receiver starts — `SyncRuntime::ensure_started` ~L391-450)
- Test: `crates/athenaeum-core/src/sharing/loopback.rs` inline + `crates/athenaeum-core/src/sharing/iroh/tests.rs`

**Interfaces:**
- Consumes: `Msg::{Offer,Want,FullHashes}` (Task 1), `DedupResponder` (Task 3).
- Produces:
  ```rust
  // SharingTransport (sharing/mod.rs)
  /// Best-effort pre-Announce dedup negotiation. Returns the rel_paths the peer still wants.
  /// Err → the caller must fall back to announcing the full package.
  async fn negotiate_want(&self, to: NodeId, package_id: PackageId,
      offer: Vec<OfferEntry>, full_by_rel: std::collections::HashMap<String, String>)
      -> anyhow::Result<std::collections::HashSet<String>> {
      let _ = (to, package_id, offer, full_by_rel);
      anyhow::bail!("negotiate_want unsupported by this transport")
  }
  // IrohTransport::new / SyncControlProtocol gain: responder: Option<Arc<dyn DedupResponder>>
  ```

- [ ] **Step 1: Write the failing test** (loopback end-to-end negotiation)

```rust
// loopback.rs tests
#[tokio::test]
async fn negotiate_returns_only_absent_and_false_positive_wants() {
    let net = LoopbackNetwork::new();
    // receiver endpoint gets a responder that "has" sampling "have" with full "F_HAVE"
    let recv = net.endpoint_with_responder(node_b, stub_responder(/* present: {"have"}, full map */));
    let send = net.endpoint(node_a);
    let offer = vec![
        OfferEntry { rel_path: "new.fits".into(), sampling_hash: "absent".into(), byte_size: 1 },
        OfferEntry { rel_path: "have.fits".into(), sampling_hash: "have".into(), byte_size: 1 },   // candidate
        OfferEntry { rel_path: "collide.fits".into(), sampling_hash: "have".into(), byte_size: 1 },// candidate, diff full
    ];
    let full: HashMap<String,String> = [("have.fits".into(),"F_HAVE".into()), ("collide.fits".into(),"F_OTHER".into())].into();
    let want = send.negotiate_want(node_b, PackageId("p".into()), offer, full).await.unwrap();
    // new.fits absent → want; have.fits true dup → dropped; collide.fits false positive → want
    assert_eq!(want, ["new.fits".to_string(), "collide.fits".to_string()].into_iter().collect());
}
```
(`stub_responder` is a test `DedupResponder` impl returning fixed present/full sets — put it in the test module; it avoids needing a real catalog here, which Task 3 already covers.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib negotiate_returns_only_absent_and_false_positive_wants` → FAIL (method absent).

- [ ] **Step 3: Implement**
  - `SharingTransport::negotiate_want` default (bail "unsupported"), so any transport that doesn't override degrades to fallback.
  - **loopback:** add `responder: Option<Arc<dyn DedupResponder>>` to `PeerInbox`; `endpoint_with_responder` (or set it on the inbox). `negotiate_want(to, …)`: look up `to`'s inbox responder; `None` → `Ok(offer.rel_paths)` (want-all, no dedup); `Some(r)` → `let (want, cands) = r.want_for_offer(&offer)`; if `cands` empty → return `want`; else build `FullHashes` entries from `cands` (rel_path, sampling_hash from the offer, xxh3_full from `full_by_rel`), `let still = r.confirm_full_hashes(&fh)`; return `want ∪ still`.
  - **iroh:** add `responder: Option<Arc<dyn DedupResponder>>` to `IrohTransport` + `SyncControlProtocol`. `negotiate_want`: a `send_request(to, Msg) -> Result<Msg>` (mirror `send_control` L287-314 but `rx.read_to_end` the **reply Msg** and `Msg::decode` it, instead of the one-byte ack). Send `Msg::Offer` → decode `Msg::Want`; if candidates non-empty send `Msg::FullHashes` → decode `Msg::Want`; assemble `want ∪ still` (still = the second Want's `want`). In the accept handler (`SyncControlProtocol::accept` L479-524), branch on the decoded Msg: `Announce`/`Ack` → emit event + write `b"1"` (unchanged); `Offer` → `responder.map(|r| r.want_for_offer(entries))` → write `Msg::Want{want, candidates}.encode()` back (if no responder, write an empty `Want` = want-nothing? No — write want-**all** so a responder-less full peer still receives everything: `Want{want: all rel_paths, candidates: []}`); `FullHashes` → `responder.confirm_full_hashes` → write `Msg::Want{want: still, candidates: []}` back. `Want` arriving inbound on the accept side is a protocol error (log + close).
  - **receiver wiring:** in `SyncRuntime::ensure_started`, construct `CatalogDedupResponder::new(store.clone())` and pass it into the transport (so a running receiver answers offers). A send-only node (no receiver) leaves `responder = None`.
  - Add `Offer`/`FullHashes`/`Want` handling so an **old** peer (that can't decode the new variant) makes `send_request` error → `negotiate_want` returns `Err` → engine fallback (verify: a decode error on the accept side must close the stream so the sender's read errors).

- [ ] **Step 4: Run** — the loopback test + an iroh two-endpoint negotiation test (mirror `iroh/tests.rs` patterns) + `cargo test -p athenaeum-core` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/ crates/athenaeum-core/src/sync/receiver.rs
git commit -m "feat(sync): negotiate_want transport primitive + responder-answered Offer/FullHashes"
```

---

### Task 5: Want-subset collection build

Serve only the want frames: build a collection from the want payloads + a **filtered manifest** (so the receiver ingests exactly the want set). Preserves Task 7 build-once (the full package dir is untouched; each engine imports only its subset).

**Files:**
- Modify: `crates/athenaeum-core/src/sharing/iroh/blobs.rs` (`import_subset_collection`)
- Modify: `crates/athenaeum-core/src/sharing/mod.rs` (`SharingTransport::serve` gains `want: Option<&HashSet<String>>`)
- Modify: `crates/athenaeum-core/src/sharing/iroh/mod.rs` + `loopback.rs` (`serve` honors the want filter)
- Test: `crates/athenaeum-core/src/sharing/iroh/tests.rs` (build a package, subset-serve, fetch, assert only want frames + a manifest with only want records land)

**Interfaces:**
- Consumes: `import_package_collection` (`iroh/blobs.rs:63`), `package::read_manifest`/`MANIFEST_FILENAME` (`package/reader.rs:14`, `package/mod.rs:47`).
- Produces:
  ```rust
  // blobs.rs — build a collection from ONLY the want rel_paths + a filtered manifest.ndjson.
  pub fn import_subset_collection(store: &Store, pkg_dir: &Path, want: &HashSet<String>, tag: &str) -> Result<Hash>;
  // SharingTransport::serve gains a want filter (None = full package, current behavior)
  async fn serve(&self, pkg: &PackageAnnounce, src_dir: &Path, want: Option<&HashSet<String>>) -> Result<()>;
  ```

- [ ] **Step 1: Write the failing test**

```rust
// iroh/tests.rs (or blobs tests)
#[tokio::test]
async fn subset_serve_transfers_only_want_frames() {
    // build a 3-frame package on disk (helper), want = {frame 1, frame 3}
    // subset-serve → fetch to a dest dir → assert: dest has frame1+frame3 payloads, NOT frame2,
    // and the fetched manifest.ndjson has exactly 2 records (frame1, frame3).
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib subset_serve_transfers_only_want_frames` → FAIL.

- [ ] **Step 3: Implement**
  - `import_subset_collection`: `read_manifest(pkg_dir)` → keep records whose `rel_path ∈ want`; write the filtered records to a temp `manifest.ndjson` (a sibling temp dir or a `tempfile`), then build the collection from `[temp filtered manifest] + [want payload files under pkg_dir]` (reuse `import_package_collection`'s add-path + `Collection::from_iter` mechanics; the manifest entry name stays `MANIFEST_FILENAME`, payload names stay their `rel_path`). Recompute nothing else — content-addressing yields a self-consistent subset `root_hash`. Guard: `want` empty → caller must not serve (Task 6 handles all-duplicate before serve); still, return a clear error if called with an empty want.
  - `serve(pkg, src_dir, want)`: `None` → `import_package_collection` (current path); `Some(w)` → `import_subset_collection(store, src_dir, w, tag)`. Loopback `serve`: for the subset case, stash `(src_dir, want)` so `fetch` copies only the want files + a filtered manifest (mirror the real subset semantics so the loopback test is faithful).
  - Update the one existing `serve` call site in the engine (Task 6 passes the want; until then pass `None` to keep it compiling — Task 6 wires the real want).

- [ ] **Step 4: Run** — the subset test + `cargo test -p athenaeum-core` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sharing/
git commit -m "feat(sync): want-subset collection build — serve only the negotiated frames + filtered manifest"
```

---

### Task 6: Engine splice — offer, negotiate, best-effort fallback, subset serve, outcome

Wire the handshake into the sender. When first building a package's announce, compute the offer (sampling hashes from the package-dir payloads), `negotiate_want`, and: empty want → terminalize as all-duplicate (no announce); non-empty → serve the want subset + announce; `Err` → announce the full package. Report `{new, duplicate}` on the finished event.

**Files:**
- Modify: `crates/athenaeum-core/src/sync/engine.rs` (`Worker::attempt` ~L572-664; `announce_for_dir`; the finished/outcome payload)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs` **only if** the finished event payload type is shared (add `new`/`duplicate` counts to the sender-side finished event; keep receiver-side unchanged)
- Test: `crates/athenaeum-core/src/sync/engine_tests.rs`

**Interfaces:**
- Consumes: `negotiate_want` (Task 4), `serve(…, Some(want))` (Task 5), `duplicates::compute_xxhash` (`duplicates/mod.rs:14`), `package::read_manifest` (manifest gives `rel_path` + full `xxh3`).
- Produces: the send path now negotiates before announce; a batch outcome `{ new: usize, duplicate: usize }` on the sender finished event.

- [ ] **Step 1: Write the failing tests**

```rust
// engine_tests.rs — loopback-backed engine with a responder on the receiver side.
#[tokio::test]
async fn all_duplicate_package_terminalizes_without_announce() {
    // receiver responder reports every offered frame as a true duplicate → want empty.
    // enqueue a package → assert it reaches a terminal "confirmed/all-duplicate" state,
    // no Announce is delivered to the receiver, finished event reports { new: 0, duplicate: n }.
}
#[tokio::test]
async fn negotiate_error_falls_back_to_full_announce() {
    // receiver has NO responder / negotiate errors → the FULL package is announced+served
    // (current behavior); receiver ingests all frames.
}
#[tokio::test]
async fn mixed_batch_serves_only_want_subset() {
    // responder wants 1 of 2 frames → only that frame is fetched; finished { new:1, duplicate:1 }.
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --lib all_duplicate_package_terminalizes_without_announce negotiate_error_falls_back_to_full_announce mixed_batch_serves_only_want_subset` → FAIL.

- [ ] **Step 3: Implement** — in `Worker::attempt`, at the point announce is first built (announce `None`, ~L585-598):
  1. `let records = package::read_manifest(&dir)?;` build `offer: Vec<OfferEntry>` — for each record, `sampling_hash = duplicates::compute_xxhash(&dir.join(&record.rel_path))?`, `byte_size = record.byte_size`; and `full_by_rel: HashMap<rel_path, record.xxh3>`.
  2. `let package_id = <the freshly minted id>;` (from `announce_for_dir`).
  3. `let want: Option<HashSet<String>> = match self.transport.negotiate_want(self.peer, package_id.clone(), offer, full_by_rel).await { Ok(w) => Some(w), Err(e) => { tracing::debug!(error=%e, "dedup negotiate failed; full send"); None } };`
  4. If `Some(w)` and `w.is_empty()` → **all-duplicate terminal**: record `{new:0, duplicate: records.len()}`, drive the package straight to a terminal confirmed state (append a confirmed-history row with per-frame `Duplicate` receipts — reuse the existing confirmed path so the cleanup coordinator + status see a normal terminal), emit the finished event, and return WITHOUT announce/serve.
  5. Else compute the announce for the served set (full when `None`, want-subset byte_size/frame_count when `Some(w)`), `self.transport.serve(&announce, &dir, want.as_ref()).await?`, `self.transport.announce(self.peer, &announce).await?` (existing block, now want-aware). Record `{new: served_count, duplicate: records.len() - served_count}` for the eventual finished event.
  - Cache the negotiated announce in `Pending.announce` so retries reuse it (do NOT re-negotiate on every retry — negotiate only when announce is `None`, matching the existing reuse pattern).
  - Keep it best-effort: a `compute_xxhash` failure on any offer entry → skip the handshake for the whole package (full send). Never fail the send because of the handshake.
  - Thread `{new, duplicate}` onto the sender finished event payload (extend the struct; regenerate `models.ts` if that struct is TS-exported — check `ts_export.rs`; if it is, run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit `models.ts`).

- [ ] **Step 4: Run** — the three tests + `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/sync/engine.rs crates/athenaeum-core/src/sync/receiver.rs
git commit -m "feat(sync): negotiate before announce — serve want-subset, all-duplicate terminal, {new,duplicate} outcome"
```

---

### Task 7: End-to-end dedup over overlapping catalogs

A two-instance `sync_e2e` test proving a re-send of an overlapping batch transfers only the new frames.

**Files:**
- Modify: `crates/athenaeum-core/tests/sync_e2e.rs` (new test, reuse the existing two-instance harness)

**Interfaces:**
- Consumes: the full send path (Task 6) + receiver responder (Task 4).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn resend_transfers_only_new_frames() {
    // Instance A sends a 3-frame batch to instance B → B ingests all 3.
    // A sends an OVERLAPPING 4-frame batch (3 same + 1 new) to B.
    // Assert: the second transfer moves only the 1 new frame (dedup dropped 3),
    //         the finished event reports { new:1, duplicate:3 }, and B ends with 4 frames total,
    //         no duplicates in B's catalog.
}
```
(Reuse `sync_e2e.rs`'s existing instance/dial harness — the one that already drives an `enqueue_sync_selection` between two real engines. Make instance B run a receiver so it has a responder.)

- [ ] **Step 2: Run, confirm fail** — `cargo test -p athenaeum-core --test sync_e2e resend_transfers_only_new_frames` → FAIL (before Tasks 1-6 land it can't compile; within SDD it runs after them, so it should drive out any integration gap).

- [ ] **Step 3: Implement** — no production code; if the test surfaces a wiring gap (e.g. instance B's transport wasn't given a responder), fix it in the relevant Task-4/6 seam and note it.

- [ ] **Step 4: Run** — `cargo test -p athenaeum-core --test sync_e2e` + the full `cargo test -p athenaeum-core` + `cargo build --workspace` green, warning-free.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/tests/sync_e2e.rs
git commit -m "test(sync): e2e — re-send of an overlapping batch transfers only new frames"
```

---

## Self-Review

**Spec coverage:**
- §3 protocol (Offer/Want/FullHashes) → Task 1. §3 receiver split (want vs candidate) + full-hash confirm → Tasks 2 (pure) + 3 (catalog). §4 send flow (negotiate before announce, best-effort fallback, all-duplicate terminal) → Task 6. §4 want-subset serve → Task 5. §2 in-band best-effort/no-ALPN-bump → Tasks 4 (Err on old peer) + 6 (fallback). §5 `{new,duplicate}` outcome → Task 6. §6 `sync/dedup.rs` + `frames_with_content_hash` helper → Tasks 2 + 3. §7 path safety unchanged (nothing written from offer/want; `validate_rel_path` on fetch/ingest untouched) — no task needed, preserved. §7 cleanup-coordinator interaction (all-duplicate terminalizes normally) → Task 6 step 4. §8 tests → each task + Task 7 e2e.
- Minimal-serve (owner decision, §2): Task 5 builds a want-subset collection from the untouched build-once dir; the Task-7 cleanup coordinator is not modified.

**Placeholder scan:** the test helpers in Tasks 3/4/6/7 point at concrete existing harnesses (`sync/store.rs`, `iroh/tests.rs`, `sync_e2e.rs`, `ingest_tests.rs` fixtures) rather than inventing one; the pure logic (Tasks 1-2) and the responder/db helper (Task 3) carry full code. Task 6's finished-event `{new,duplicate}` explicitly checks whether the struct is TS-exported before regenerating.

**Type consistency:** `OfferEntry`/`FullHashEntry` (Task 1) are the wire + pure-logic + responder types used unchanged in Tasks 2-6. `partition_offer`/`confirm_candidates` (Task 2) are called only by `CatalogDedupResponder` (Task 3), which is the `DedupResponder` the transport (Task 4) invokes. `negotiate_want(to, package_id, Vec<OfferEntry>, HashMap<String,String>) -> HashSet<String>` (Task 4) is called once by the engine (Task 6). `serve(pkg, src_dir, want: Option<&HashSet<String>>)` (Task 5) is called by the engine (Task 6) and back-filled to `None` at the existing call site in Task 5 so the tree compiles between tasks.

**Ordering:** 1 (types) → 2 (pure) → 3 (catalog responder) → 4 (transport negotiate) → 5 (subset serve) → 6 (engine wires 4+5) → 7 (e2e). Each task ends green on `athenaeum-core`; `cargo build --workspace` stays green throughout (no cross-crate contract changes — Perseus/app consume `SyncEngine` unchanged).

---

## Execution Handoff

Execute with **superpowers:subagent-driven-development** (owner's standing choice): fresh rust-engineer + opus implementer per task, opus reviewer after each, broad review at the end. Ledger: `.superpowers/sdd/progress-3.md`. Core-only; no deploy (Plan 3 rides the same held joint-hub ship as Plan 2 — but adds no hub dependency, so it can also ship independently later if desired).
