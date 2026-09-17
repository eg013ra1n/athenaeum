//! Dev probe: time each step of the scanner's Phase 4 (duplicate-cache
//! rebuild) against a COPY of a real catalog. Never point it at the live DB.
//!
//! cargo run --release -p athenaeum-core --example phase4_probe -- <copy.db> [--hash]
//!
//! `--hash` also runs the master strong-hash pass (touches the files the
//! catalog names — only meaningful when those volumes are mounted).

use athenaeum_core::db::{
    find_duplicate_folders, rebuild_duplicate_groups_cache, rebuild_folder_similarity_cache,
    DuplicateKey,
};
use athenaeum_core::events::NullEmitter;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: phase4_probe <copy.db> [--hash]");
    let do_hash = args.iter().any(|a| a == "--hash");
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").unwrap();

    let t = Instant::now();
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files f
             JOIN fits_header fh ON fh.file_id = f.id
             JOIN frames fr ON fr.file_id = f.id
             WHERE (f.strong_hash IS NULL OR f.strong_hash = '')
               AND fh.header_fingerprint IS NOT NULL AND fh.header_fingerprint <> ''
               AND (COALESCE(fr.is_master, 0) = 1
                    OR fr.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat'))
               AND EXISTS (
                  SELECT 1 FROM files f2
                  JOIN fits_header fh2 ON fh2.file_id = f2.id
                  JOIN frames fr2 ON fr2.file_id = f2.id
                  WHERE f2.id <> f.id AND f2.size = f.size
                    AND fh2.header_fingerprint = fh.header_fingerprint
                    AND (COALESCE(fr2.is_master, 0) = 1
                         OR fr2.imagetyp NOT IN ('Light','Flat','Dark','Bias','DarkFlat')))",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("backfill pending SELECT: {} rows in {:?}", pending, t.elapsed());

    if do_hash {
        let t = Instant::now();
        let n = athenaeum_core::duplicates::backfill::fill_master_strong_hashes(
            &conn,
            &NullEmitter,
            &AtomicBool::new(false),
            0,
        );
        println!("fill_master_strong_hashes: wrote {} in {:?}", n, t.elapsed());
    }

    for key in [DuplicateKey::Header, DuplicateKey::Master] {
        let t = Instant::now();
        let n = rebuild_duplicate_groups_cache(&conn, key).unwrap();
        println!("rebuild_duplicate_groups_cache({:?}): {} groups in {:?}", key, n, t.elapsed());
    }

    let t = Instant::now();
    let sims = find_duplicate_folders(&conn, 70.0).unwrap();
    println!("find_duplicate_folders: {} pairs in {:?}", sims.len(), t.elapsed());

    let t = Instant::now();
    let n = rebuild_folder_similarity_cache(&conn, 70.0).unwrap();
    println!("rebuild_folder_similarity_cache: {} rows in {:?}", n, t.elapsed());

    let folders: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT substr(path, 1, length(path) - length(filename) - 1)) FROM files",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("distinct folders in catalog: {}", folders);
}
