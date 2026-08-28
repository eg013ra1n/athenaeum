//! Catalog-backed receive-side dedup responder (spec §7).
//!
//! The transport's receive half ([task 4](super)) calls a [`DedupResponder`] to
//! answer a provider's [`Offer`](crate::sharing::iroh::proto::Msg::Offer): first
//! [`want_for_offer`](DedupResponder::want_for_offer) splits the offered frames
//! into *definite wants* (sampling hash absent from the local catalog) and
//! *candidates* (sampling hash present — a possible duplicate). Then, once the
//! provider answers those candidates with full-file digests, the receiver calls
//! [`confirm_full_hashes`](DedupResponder::confirm_full_hashes) to drop the ones
//! whose FULL xxh3 matches a local file (true duplicate) and keep the sampling
//! false-positives.
//!
//! The pure split lives in [`crate::sync::dedup`]; this module supplies the
//! catalog membership and the on-disk full-hash re-computation around it, and is
//! the sole place that touches the filesystem (`xxh3_full_file`). Every error
//! path — a stale catalog row, a file gone from disk, an unreadable file —
//! resolves toward **keeping the candidate wanted**, the safe direction: a
//! redundant resend costs bandwidth, a wrongly-dropped frame loses data.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::db::SamplingHashMatch;
use crate::duplicates::backfill::disk_matches_row;
use crate::package::xxh3_full_file;
use crate::sharing::iroh::proto::{FullHashEntry, OfferEntry};
use crate::sync::dedup::{confirm_candidates, partition_offer};
use crate::sync::store::CatalogSyncStore;

/// The receive-side dedup decision surface the transport drives during the
/// offer/want/full-hashes handshake.
pub trait DedupResponder: Send + Sync {
    /// Split an incoming offer into `(want, candidates)` rel_paths: `want` =
    /// offered frames whose sampling hash is absent locally (definitely new),
    /// `candidates` = sampling hash present (possible duplicate, needs a
    /// full-hash confirm). Input order is preserved within each bucket.
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>);

    /// Given the provider's full-file digests for the candidates, return the
    /// rel_paths that are STILL wanted — the sampling-hash collisions whose full
    /// hash does NOT match any local file. A true full-hash match is dropped.
    fn confirm_full_hashes(&self, entries: &[FullHashEntry]) -> Vec<String>;
}

/// [`DedupResponder`] backed by the app catalog: sampling-hash membership comes
/// from `files.content_hash`, full-hash confirmation re-hashes the matching
/// local files from disk with [`xxh3_full_file`].
pub struct CatalogDedupResponder {
    store: Arc<CatalogSyncStore>,
}

impl CatalogDedupResponder {
    pub fn new(store: Arc<CatalogSyncStore>) -> Self {
        Self { store }
    }

    /// The catalog rows whose sampling hash is one of `hashes`. A DB error is
    /// logged and treated as "nothing present" — the safe direction
    /// (everything becomes a want / stays wanted).
    fn present_sampling(&self, hashes: &[String]) -> Vec<SamplingHashMatch> {
        match self.store.files_by_sampling_hashes(hashes) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "dedup responder catalog lookup failed; treating as absent");
                Vec::new()
            }
        }
    }
}

impl DedupResponder for CatalogDedupResponder {
    fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
        let offered: Vec<String> = entries.iter().map(|e| e.sampling_hash.clone()).collect();
        let present: HashSet<String> = self
            .present_sampling(&offered)
            .into_iter()
            .map(|m| m.content_hash)
            .collect();
        let split = partition_offer(entries, &present);
        tracing::debug!(
            offered = entries.len(),
            want = split.want.len(),
            candidates = split.candidates.len(),
            "dedup responder partitioned offer"
        );
        (split.want, split.candidates)
    }

    fn confirm_full_hashes(&self, entries: &[FullHashEntry]) -> Vec<String> {
        let sampling: Vec<String> = entries.iter().map(|e| e.sampling_hash.clone()).collect();
        let rows = self.present_sampling(&sampling);

        // sampling_hash → set of FULL xxh3 of the local files carrying it. A
        // file missing on disk / unreadable is skipped (no local full hash), so
        // its candidate stays wanted — the safe direction.
        //
        // The full hash is the expensive part, so it is not thrown away: a row
        // the disk still vouches for (`disk_matches_row`, the contract shared
        // with the master-hash pass and deep verify) BANKS the digest it just
        // read into `files.strong_hash`, and a row that already carries one is
        // decided from it without reading at all — a re-offer of files
        // confirmed once costs zero bytes of I/O. A stale row is still decided
        // by reading (the question is what is on disk) but never written.
        let mut local_full_by_sampling: HashMap<String, HashSet<String>> = HashMap::new();
        let mut skipped = 0usize;
        let mut banked = 0usize;
        let mut reused = 0usize;
        for m in rows {
            let path = std::path::Path::new(&m.path);
            let current = disk_matches_row(path, m.size, &m.modified_at);
            let full = match (&m.strong_hash, current) {
                (Some(stored), true) => {
                    reused += 1;
                    stored.clone()
                }
                _ => match xxh3_full_file(path) {
                    Ok(full) => {
                        if current {
                            match self.store.bank_strong_hash(m.file_id, &full) {
                                Ok(()) => banked += 1,
                                // The verdict stands either way — losing the
                                // banked hash costs a re-read later, never a
                                // wrong answer now.
                                Err(e) => tracing::error!(
                                    file_id = m.file_id, error = %e,
                                    "dedup confirm: strong_hash write failed"
                                ),
                            }
                        }
                        full
                    }
                    Err(e) => {
                        skipped += 1;
                        tracing::debug!(path = %m.path, error = %e, "skipping unhashable local file in dedup confirm");
                        continue;
                    }
                },
            };
            local_full_by_sampling
                .entry(m.content_hash)
                .or_default()
                .insert(full);
        }

        let still = confirm_candidates(entries, &local_full_by_sampling);
        tracing::debug!(
            candidates = entries.len(),
            still_wanted = still.len(),
            skipped_files = skipped,
            banked,
            reused,
            "dedup responder confirmed candidate full hashes"
        );
        still
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Build a `CatalogSyncStore` over a fresh temp catalog seeded with `files`
    /// rows. Each `(rel_name, sampling_hash, contents)` writes a real payload
    /// file under `tmp` (so `xxh3_full_file` returns a known value) and inserts
    /// a matching `files` row (`content_hash` = the sampling hash). Returns the
    /// store plus the on-disk path of each written file, in input order.
    fn test_store_with_files(
        tmp: &Path,
        specs: &[(&str, &str, &[u8])],
    ) -> (Arc<CatalogSyncStore>, Vec<std::path::PathBuf>) {
        let catalog_path = tmp.join("catalog.db");
        // `Database::new` runs `init_db` (creates `files` + the sync tables).
        let db = crate::db::Database::new(catalog_path.clone()).unwrap();
        let mut paths = Vec::new();
        {
            let conn = db.conn();
            for (rel_name, sampling_hash, contents) in specs {
                let path = tmp.join(rel_name);
                std::fs::write(&path, contents).unwrap();
                let path_str = path.to_string_lossy().to_string();
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                     VALUES (?1, ?2, ?3, '2026-07-11T00:00:00Z', 'FITS', '2026-07-11T00:00:00Z', ?4)",
                    rusqlite::params![path_str, rel_name, contents.len() as i64, sampling_hash],
                )
                .unwrap();
                paths.push(path);
            }
        }
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        (store, paths)
    }

    fn oe(rel: &str, sampling: &str) -> OfferEntry {
        OfferEntry {
            rel_path: rel.into(),
            sampling_hash: sampling.into(),
            byte_size: 1,
        }
    }

    #[tokio::test]
    async fn responder_wants_absent_and_confirms_candidates() {
        let tmp = tempfile::TempDir::new().unwrap();
        // One local file with a known sampling hash and real on-disk contents.
        let (store, paths) =
            test_store_with_files(tmp.path(), &[("have.fits", "samp-have", b"local file payload")]);
        let local_full = xxh3_full_file(&paths[0]).unwrap();

        let r = CatalogDedupResponder::new(Arc::clone(&store));

        // want_for_offer: absent sampling → want; present sampling → candidate.
        let (want, cands) = r.want_for_offer(&[
            oe("new.fits", "samp-absent"),
            oe("have.fits", "samp-have"),
        ]);
        assert_eq!(want, vec!["new.fits".to_string()]);
        assert_eq!(cands, vec!["have.fits".to_string()]);

        // confirm_full_hashes: same full hash as the local file → dropped.
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "have.fits".into(),
            sampling_hash: "samp-have".into(),
            xxh3_full: local_full.clone(),
        }]);
        assert!(still.is_empty(), "true full-hash duplicate must be dropped");

        // A candidate that shares the sampling hash but has a DIFFERENT full
        // hash is a false positive → still wanted.
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "have.fits".into(),
            sampling_hash: "samp-have".into(),
            xxh3_full: "0000000000000000".into(),
        }]);
        assert_eq!(
            still,
            vec!["have.fits".to_string()],
            "sampling-hash collision with a differing full hash stays wanted"
        );
    }

    /// Task E end-to-end at the responder level: a `files` row created the way
    /// the scanner now always creates it — `content_hash` = the real
    /// `duplicates::compute_xxhash` of a payload on disk — must dedup against a
    /// sender whose `OfferEntry.sampling_hash` is the SAME `compute_xxhash`
    /// (`engine::build_offer`). `want_for_offer` classifies it a candidate;
    /// `confirm_full_hashes` drops it on the matching full hash → zero bytes
    /// travel for a file already in the scanned library.
    #[tokio::test]
    async fn scanner_hashed_library_file_dedups_at_responder() {
        let tmp = tempfile::TempDir::new().unwrap();
        let payload_path = tmp.path().join("light.fits");
        // Larger than one sampling chunk so compute_xxhash reads all three
        // positions (matches a real frame, not a single-window file).
        std::fs::write(&payload_path, vec![0xABu8; 2 * 1024 * 1024]).unwrap();

        // Scanner-style content_hash: the exact fn the scanner now always runs.
        let sampling_hash = crate::duplicates::compute_xxhash(&payload_path).unwrap();
        let full_hash = xxh3_full_file(&payload_path).unwrap();

        let catalog_path = tmp.path().join("catalog.db");
        let db = crate::db::Database::new(catalog_path.clone()).unwrap();
        {
            let conn = db.conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
                 VALUES (?1, 'light.fits', ?2, '2026-07-11T00:00:00Z', 'FITS', '2026-07-11T00:00:00Z', ?3)",
                rusqlite::params![
                    payload_path.to_string_lossy().to_string(),
                    2i64 * 1024 * 1024,
                    sampling_hash,
                ],
            )
            .unwrap();
        }
        let store = Arc::new(CatalogSyncStore::open(&catalog_path).unwrap());
        let r = CatalogDedupResponder::new(store);

        // Sampling hash present in the catalog → candidate, never a fresh want.
        let (want, cands) = r.want_for_offer(&[oe("light.fits", &sampling_hash)]);
        assert!(want.is_empty(), "a scanned library file must not be re-wanted");
        assert_eq!(cands, vec!["light.fits".to_string()], "sampling-hash match → candidate");

        // Full-hash confirm with the real full digest drops it → true duplicate.
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "light.fits".into(),
            sampling_hash: sampling_hash.clone(),
            xxh3_full: full_hash,
        }]);
        assert!(
            still.is_empty(),
            "scanner-hashed library file confirmed as duplicate → dropped, zero bytes transfer"
        );
    }

    #[tokio::test]
    async fn missing_local_file_keeps_candidate_wanted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, paths) =
            test_store_with_files(tmp.path(), &[("gone.fits", "samp-gone", b"soon deleted")]);
        // Delete the on-disk payload but leave the catalog row (stale row).
        std::fs::remove_file(&paths[0]).unwrap();
        let r = CatalogDedupResponder::new(store);

        // Even sending the (former) real full hash cannot confirm — the file is
        // gone, so we cannot re-hash it and must keep the candidate wanted.
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "gone.fits".into(),
            sampling_hash: "samp-gone".into(),
            xxh3_full: "1234567812345678".into(),
        }]);
        assert_eq!(
            still,
            vec!["gone.fits".to_string()],
            "an unhashable local file must never silently drop a candidate"
        );
    }

    #[tokio::test]
    async fn empty_offer_yields_empty_split() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (store, _paths) = test_store_with_files(tmp.path(), &[]);
        let r = CatalogDedupResponder::new(store);
        let (want, cands) = r.want_for_offer(&[]);
        assert!(want.is_empty());
        assert!(cands.is_empty());
        assert!(r.confirm_full_hashes(&[]).is_empty());
    }

    /// Insert a `files` row whose `(size, modified_at)` are the file's REAL
    /// stat — a row the staleness contract (`disk_matches_row`) accepts, so a
    /// full hash read for the dedup confirm may be banked into `strong_hash`.
    /// `test_store_with_files` deliberately stores a fixed mtime (a stale row).
    fn insert_current_row(catalog: &Path, path: &Path, sampling_hash: &str) -> i64 {
        let meta = std::fs::metadata(path).unwrap();
        let mtime = chrono::DateTime::<chrono::Utc>::from(meta.modified().unwrap()).to_rfc3339();
        let db = crate::db::Database::new(catalog.to_path_buf()).unwrap();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at, content_hash)
             VALUES (?1, ?2, ?3, ?4, 'FITS', '2026-07-11T00:00:00Z', ?5)",
            rusqlite::params![
                path.to_string_lossy().to_string(),
                path.file_name().unwrap().to_string_lossy().to_string(),
                meta.len() as i64,
                mtime,
                sampling_hash,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn stored_strong_hash(catalog: &Path, id: i64) -> Option<String> {
        let db = crate::db::Database::new(catalog.to_path_buf()).unwrap();
        let conn = db.conn();
        conn.query_row("SELECT strong_hash FROM files WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    /// The confirm step reads a candidate's whole file to compare full hashes;
    /// that read is banked into `files.strong_hash` when the row still
    /// describes the bytes on disk — the same contract as the master-hash
    /// pass and deep verify, so the Master duplicate key and a later verify
    /// get the digest for free.
    #[tokio::test]
    async fn confirm_banks_the_full_hash_into_a_current_row() {
        let tmp = tempfile::TempDir::new().unwrap();
        let payload = tmp.path().join("have.fits");
        std::fs::write(&payload, vec![0x5Cu8; 300_000]).unwrap();
        let catalog = tmp.path().join("catalog.db");
        let id = insert_current_row(&catalog, &payload, "samp-have");
        assert_eq!(stored_strong_hash(&catalog, id), None, "precondition: nothing banked yet");

        let full = xxh3_full_file(&payload).unwrap();
        let r = CatalogDedupResponder::new(Arc::new(CatalogSyncStore::open(&catalog).unwrap()));
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "have.fits".into(),
            sampling_hash: "samp-have".into(),
            xxh3_full: full.clone(),
        }]);
        assert!(still.is_empty(), "true duplicate is dropped");
        assert_eq!(
            stored_strong_hash(&catalog, id).as_deref(),
            Some(full.as_str()),
            "the read the confirm already paid for is banked"
        );
    }

    /// A row that lies about its bytes (`size` drifted) is still CONFIRMED by
    /// reading the file — the decision is about what is on disk — but nothing
    /// is banked into it: a fresh-content hash on a stale row would let a later
    /// reader trust a digest the scanner never vouched for.
    #[tokio::test]
    async fn confirm_does_not_bank_into_a_stale_row() {
        let tmp = tempfile::TempDir::new().unwrap();
        let payload = tmp.path().join("stale.fits");
        std::fs::write(&payload, vec![0x5Cu8; 300_000]).unwrap();
        let catalog = tmp.path().join("catalog.db");
        let id = insert_current_row(&catalog, &payload, "samp-stale");
        {
            let db = crate::db::Database::new(catalog.clone()).unwrap();
            db.conn()
                .execute("UPDATE files SET size = size + 1 WHERE id = ?1", [id])
                .unwrap();
        }

        let full = xxh3_full_file(&payload).unwrap();
        let r = CatalogDedupResponder::new(Arc::new(CatalogSyncStore::open(&catalog).unwrap()));
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "stale.fits".into(),
            sampling_hash: "samp-stale".into(),
            xxh3_full: full,
        }]);
        assert!(still.is_empty(), "the bytes on disk match, so the candidate is still dropped");
        assert_eq!(stored_strong_hash(&catalog, id), None, "a stale row is never written");
    }

    /// A current row that already carries `strong_hash` decides the confirm
    /// WITHOUT reading the file: the payload is made unreadable (stat still
    /// works, open does not), and the candidate is still dropped on the banked
    /// digest. A re-offer of files already confirmed once costs zero reads.
    #[cfg(unix)]
    #[tokio::test]
    async fn confirm_uses_a_banked_hash_without_reading() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let payload = tmp.path().join("banked.fits");
        std::fs::write(&payload, vec![0x5Cu8; 300_000]).unwrap();
        let full = xxh3_full_file(&payload).unwrap();
        let catalog = tmp.path().join("catalog.db");
        let id = insert_current_row(&catalog, &payload, "samp-banked");
        {
            let db = crate::db::Database::new(catalog.clone()).unwrap();
            db.conn()
                .execute("UPDATE files SET strong_hash = ?2 WHERE id = ?1", rusqlite::params![id, full])
                .unwrap();
        }
        // Unreadable, but stat-able: a read attempt would fail and (by the
        // existing rule) keep the candidate wanted — so a drop proves the
        // banked digest decided it.
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&payload).is_ok() {
            // Running as root: the permission bits do not bite, the probe is
            // meaningless. Restore and bail rather than pass vacuously.
            std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o644)).unwrap();
            eprintln!("skipping confirm_uses_a_banked_hash_without_reading: file readable despite 0o000 (root)");
            return;
        }

        let r = CatalogDedupResponder::new(Arc::new(CatalogSyncStore::open(&catalog).unwrap()));
        let still = r.confirm_full_hashes(&[FullHashEntry {
            rel_path: "banked.fits".into(),
            sampling_hash: "samp-banked".into(),
            xxh3_full: full,
        }]);
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(still.is_empty(), "the banked digest must decide the confirm without a read");
    }
}
