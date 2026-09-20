//! Dev probe: reproducible before/after throughput bench for the iroh swarm
//! fetch (`SharingTransport::fetch_collection_multi`), written to gate the
//! iroh 1.0.3 -> 1.2.0 bump (Collaboration v2 Wave 1, task A7).
//!
//! Binds three iroh endpoints in-process (relay disabled, direct localhost
//! addresses) — the same shape as
//! `sharing::iroh::tests::multi_fetch_uses_both_providers`, whose helpers
//! (`build_many_file_package`) are duplicated below rather than imported:
//! that module is `#[cfg(test)]`-gated, so an example (a separate binary
//! target) cannot see it.
//!
//! A and B serve a package, C pulls it — once with A alone (`single`), once
//! with A+B together (`multi`). Each run gets its OWN package content (a
//! different `prefix`, hence a different iroh collection hash and a fresh
//! destination directory) so the second run can never be served out of C's
//! own local blob store instead of the network — which would silently make
//! "multi" look faster for a reason that has nothing to do with having two
//! providers.
//!
//! `write_package`'s default `root_hash` is an xxh3 placeholder, not the real
//! iroh-blobs collection hash `fetch_collection_multi` needs (see
//! `package::writer::RootHashProvider`'s doc comment) — the real hash is only
//! known after `serve()` imports the directory into a node's blob store.
//! There is no public accessor for it (the test suite's own
//! `resolve_served_hash_for_test` is `#[cfg(test)]` + `pub(crate)`), so this
//! probe learns it the same way a real receiver does: A `announce()`s the
//! package to C over the wire (which substitutes the registered collection
//! hash before sending, see `RoleHandle::announce`), and C reads it back off
//! the `TransportEvent::AnnounceReceived` it gets from `events()`.
//!
//! cargo run -p athenaeum-core --release --example swarm_bench -- \
//!   --label before [--files 12] [--size-mb 32] [--out target/swarm-bench]
//!
//! Writes `<out>/<label>.json` (`{ label, iroh, files, size_mb,
//! single_mb_s, multi_two_providers_mb_s, elapsed_single_ms,
//! elapsed_multi_ms }`) and prints the same line. `RUST_LOG`-gated
//! subscriber like the other probes; examples are exempt from the
//! zero-`println!` rule.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use athenaeum_core::package::{self, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION};
use athenaeum_core::sharing::iroh::node::{Role, SharedIrohNode};
use athenaeum_core::sharing::{
    noop_fetch_sink, noop_provider_telemetry, NodeId, PackageAnnounce, PackageLayout,
    SharingTransport, TransportEvent,
};

use iroh::RelayMode;
use tempfile::tempdir;

/// Keep in step with the `iroh = "=…"` pin in `Cargo.toml` — an example has
/// no runtime way to read a dependency crate's own version.
const IROH_VERSION: &str = "1.2.0";

struct Args {
    label: String,
    files: usize,
    size_mb: usize,
    out: PathBuf,
}

fn usage() -> ! {
    eprintln!(
        "usage: swarm_bench --label <name> [--files N] [--size-mb N] [--out DIR]\n\
         defaults: --files 12 --size-mb 32 --out target/swarm-bench"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut label: Option<String> = None;
    let mut files = 12usize;
    let mut size_mb = 32usize;
    let mut out = PathBuf::from("target/swarm-bench");
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--label" => label = Some(it.next().unwrap_or_else(|| usage())),
            "--files" => {
                files = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--size-mb" => {
                size_mb = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--out" => out = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    Args {
        label: label.unwrap_or_else(|| usage()),
        files,
        size_mb,
        out,
    }
}

async fn bind_disabled(dir: &Path) -> Arc<SharedIrohNode> {
    SharedIrohNode::bind(dir, RelayMode::Disabled)
        .await
        .expect("bind relay-disabled node")
}

/// Copied from `sharing::iroh::tests::build_many_file_package` (that module
/// is `#[cfg(test)]`-only and invisible to an example), with one change: the
/// byte pattern is also seeded from `prefix`. The original helper's pattern
/// depends only on the file index, so two calls with different `prefix`es
/// but the same `files`/`size` produce BYTE-IDENTICAL payload blobs — which
/// iroh-blobs content-addresses, so the second run's fetch would find every
/// child already in the puller's local store and finish in milliseconds,
/// measuring a cache hit instead of a real transfer (the first version of
/// this probe did exactly that: a 4 ms "multi" run). Mixing `prefix` into
/// the pattern gives each run's package its own blob content, hence its own
/// collection hash, with nothing to dedupe against.
fn build_many_file_package(
    src_root: &Path,
    prefix: &str,
    files: usize,
    size: usize,
) -> (PathBuf, PackageAnnounce) {
    std::fs::create_dir_all(src_root).unwrap();
    let seed: u64 = prefix
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(131).wrapping_add(b as u64));
    let mut records = Vec::with_capacity(files);
    for i in 0..files {
        let name = format!("frame_{i:04}.fits");
        let payload = src_root.join(&name);
        let bytes: Vec<u8> = (0..size)
            .map(|j| ((j as u64 + i as u64 * 97 + seed) % 251) as u8)
            .collect();
        std::fs::write(&payload, &bytes).unwrap();
        let byte_size = std::fs::metadata(&payload).unwrap().len();
        let xxh3 = package::xxh3_full_file(&payload).unwrap();
        records.push((
            payload,
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("{prefix}-uuid-{i:04}"),
                origin_catalog_uuid: "catalog-uuid".to_string(),
                origin_device: "origin-device".to_string(),
                payload_kind: PayloadKind::RawFrame,
                rel_path: name,
                byte_size,
                xxh3,
                frame_meta: serde_json::json!({ "object": "swarm-bench" }),
                analysis: None,
                app_version: "swarm_bench".to_string(),
                project: None,
            },
        ));
    }
    let pkg_dir = src_root.parent().unwrap().join(format!("pkg-{prefix}"));
    let announce = write_package(&pkg_dir, records).unwrap();
    (pkg_dir, announce)
}

/// Wait for the next `TransportEvent::AnnounceReceived` and return the
/// announce it carried (the REAL iroh collection hash, substituted in by the
/// sender's `announce()` — see the module doc comment).
async fn recv_announce(rx: &mut tokio::sync::mpsc::Receiver<TransportEvent>) -> PackageAnnounce {
    let ev = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("event channel stalled")
        .expect("event channel closed unexpectedly");
    match ev {
        TransportEvent::AnnounceReceived { announce, .. } => announce,
        other => panic!("expected AnnounceReceived, got {other:?}"),
    }
}

/// Serve a package (fresh content keyed by `label`) on every one of
/// `providers`, learn its real collection hash by announcing it from the
/// first provider to `puller`, then time a `fetch_collection_multi` pulling
/// from all of `providers` at once. Returns `(bytes_fetched, elapsed)`.
async fn run_fetch(
    label: &str,
    src_root: &Path,
    files: usize,
    size: usize,
    providers: &[(Arc<dyn SharingTransport>, NodeId)],
    puller: &Arc<dyn SharingTransport>,
    puller_node_id: NodeId,
) -> (u64, Duration) {
    let (pkg_dir, announce) = build_many_file_package(src_root, label, files, size);
    for (out, _) in providers {
        out.serve(&announce, &pkg_dir, None, None)
            .await
            .expect("serve must succeed");
    }

    let mut events = puller.events().await;
    providers[0]
        .0
        .announce(
            puller_node_id,
            &announce,
            label,
            label,
            &[],
            PackageLayout::Batch,
        )
        .await
        .expect("announce must succeed");
    let real = recv_announce(&mut events).await;

    let dest = tempdir().unwrap();
    let provider_ids: Vec<NodeId> = providers.iter().map(|(_, id)| *id).collect();
    let started = Instant::now();
    puller
        .fetch_collection_multi(
            provider_ids,
            &real.root_hash,
            real.byte_size,
            dest.path(),
            noop_fetch_sink(),
            noop_provider_telemetry(),
        )
        .await
        .expect("swarm fetch must complete");
    (real.byte_size, started.elapsed())
}

fn mb_per_sec(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64().max(1e-6);
    (bytes as f64 / (1024.0 * 1024.0)) / secs
}

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }

    let args = parse_args();
    let size_bytes = args.size_mb * 1024 * 1024;

    let da = tempdir().unwrap();
    let db = tempdir().unwrap();
    let dc = tempdir().unwrap();
    let src = tempdir().unwrap();

    let a = bind_disabled(da.path()).await;
    let b = bind_disabled(db.path()).await;
    let c = bind_disabled(dc.path()).await;

    let a_out = a.handle(Role::Out);
    let b_out = b.handle(Role::Out);
    let c_recv = c.handle(Role::Recv);

    let a_info = a_out.start().await.expect("start A");
    let b_info = b_out.start().await.expect("start B");
    let c_info = c_recv.start().await.expect("start C");

    // Relay-disabled endpoints have no discovery, so addresses travel out of
    // band via pairing tickets — both directions, as in the acceptance test.
    c.add_peer_ticket(&a_info.pairing_ticket).unwrap();
    c.add_peer_ticket(&b_info.pairing_ticket).unwrap();
    a.add_peer_ticket(&c_info.pairing_ticket).unwrap();
    b.add_peer_ticket(&c_info.pairing_ticket).unwrap();

    // Single-provider run: A only.
    let (single_bytes, elapsed_single) = run_fetch(
        "single",
        &src.path().join("single-src"),
        args.files,
        size_bytes,
        &[(a_out.clone(), a_info.node_id)],
        &c_recv,
        c_info.node_id,
    )
    .await;

    // Two-provider run: A + B, DIFFERENT package content from the single run
    // (see the module doc comment on why that matters).
    let (multi_bytes, elapsed_multi) = run_fetch(
        "multi",
        &src.path().join("multi-src"),
        args.files,
        size_bytes,
        &[
            (a_out.clone(), a_info.node_id),
            (b_out.clone(), b_info.node_id),
        ],
        &c_recv,
        c_info.node_id,
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;

    let single_mb_s = mb_per_sec(single_bytes, elapsed_single);
    let multi_mb_s = mb_per_sec(multi_bytes, elapsed_multi);

    let report = serde_json::json!({
        "label": args.label,
        "iroh": IROH_VERSION,
        "files": args.files,
        "size_mb": args.size_mb,
        "single_mb_s": single_mb_s,
        "multi_two_providers_mb_s": multi_mb_s,
        "elapsed_single_ms": elapsed_single.as_millis() as u64,
        "elapsed_multi_ms": elapsed_multi.as_millis() as u64,
    });

    std::fs::create_dir_all(&args.out).expect("create --out dir");
    let out_path = args.out.join(format!("{}.json", args.label));
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&report).expect("serialize report"),
    )
    .expect("write bench json");

    println!("{report}");
    println!("wrote {}", out_path.display());
}
