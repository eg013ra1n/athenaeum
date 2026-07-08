//! Two-machine iroh transport validation harness (task A5, manual gate).
//!
//! This is the CLI the owner runs on two real machines (both behind NAT) to
//! validate the iroh stack before the sync feature graduates: NAT-traversed
//! connection, a verified + resumable collection transfer, and hash-confirmed
//! delivery. It drives the exact blob-collection code path
//! (`sharing::iroh::blobs`) the real [`IrohTransport`] uses.
//!
//! Usage:
//!   # On machine A (the sender) — serve a directory of files (e.g. a package
//!   # dir with manifest.ndjson, or any FITS folder):
//!   cargo run -p athenaeum-core --example sync_validation -- serve <dir>
//!     → prints a ticket. Leave it running.
//!
//!   # On machine B (the receiver) — fetch using the printed ticket:
//!   cargo run -p athenaeum-core --example sync_validation -- fetch <ticket> <dest_dir>
//!
//! Resume test: start a fetch, kill the network (or Ctrl-C the fetch) mid-way,
//! then re-run the SAME fetch command — the persisted blob store resumes only
//! the missing byte ranges. Delete-at-source is an app-level concern (Perseus,
//! task A6+) and is intentionally NOT exercised here.
//!
//! `println!` is used deliberately — this is a user-facing CLI (examples/ are
//! exempt from the zero-print rule).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use athenaeum_core::package;
use athenaeum_core::sharing::iroh::blobs;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "athenaeum_core=info,iroh=warn".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();

    match argv.as_slice() {
        ["serve", dir] => serve(Path::new(dir)).await,
        ["fetch", ticket, dest] => fetch(ticket, Path::new(dest)).await,
        _ => {
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    println!("iroh transport validation harness");
    println!();
    println!("USAGE:");
    println!("  serve <dir>                 import <dir> as a collection and serve it; prints a ticket");
    println!("  fetch <ticket> <dest_dir>   download the collection into <dest_dir> and verify");
    println!();
    println!("Run `serve` on machine A, copy the printed ticket to machine B, run `fetch` there.");
    println!("Both machines may be behind NAT; iroh relays broker the connection.");
}

/// Blob store lives next to the served/fetched directory so it persists across
/// restarts (that persistence is what makes a fetch resumable).
fn blob_store_dir(anchor: &Path) -> PathBuf {
    let parent = anchor.parent().unwrap_or_else(|| Path::new("."));
    parent.join(".athenaeum_sync_blobs")
}

/// Load a stable secret key from `path`, or generate + persist one so the served
/// ticket's node id survives a restart.
fn load_or_create_secret(path: &Path) -> Result<SecretKey> {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut b = [0u8; 32];
            b.copy_from_slice(&bytes);
            return Ok(SecretKey::from_bytes(&b));
        }
    }
    let sk = SecretKey::generate();
    std::fs::write(path, sk.to_bytes()).with_context(|| format!("write secret {}", path.display()))?;
    Ok(sk)
}

async fn serve(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("not a directory: {}", dir.display());
    }
    let store_dir = blob_store_dir(dir);
    let store = FsStore::load(store_dir.join("serve"))
        .await
        .context("open serve blob store")?;

    let secret = load_or_create_secret(&blob_store_dir(dir).join("serve_secret"))?;
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .relay_mode(RelayMode::Default)
        .bind()
        .await
        .context("bind endpoint")?;

    let blobs = BlobsProtocol::new(&store, None);
    let router = Router::builder(endpoint)
        .accept(iroh_blobs::ALPN, blobs)
        .spawn();

    println!("Waiting for the endpoint to come online (relay handshake)...");
    router.endpoint().online().await;

    println!("Importing {} as a collection (hashing all files)...", dir.display());
    let hash = blobs::import_package_collection(&store, dir, "pkg/validation")
        .await
        .context("import collection")?;

    let addr = router.endpoint().addr();
    let ticket = BlobTicket::new(addr, hash, BlobFormat::HashSeq);

    println!();
    println!("Collection ready. Fetch it from the other machine with:");
    println!();
    println!("  cargo run -p athenaeum-core --example sync_validation -- fetch {ticket} <dest_dir>");
    println!();
    println!("Serving. Press Enter here to stop once the other side finishes");
    println!("(or Ctrl-C the process). Kill the network mid-transfer to test resume.");

    // Block on stdin rather than pulling tokio's `signal` feature into core.
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    })
    .await
    .ok();

    println!("Shutting down...");
    router.shutdown().await.ok();
    store.shutdown().await.ok();
    Ok(())
}

async fn fetch(ticket: &str, dest: &Path) -> Result<()> {
    let ticket: BlobTicket = ticket.parse().context("parse ticket")?;
    let store_dir = blob_store_dir(dest);
    let store = FsStore::load(store_dir.join("fetch"))
        .await
        .context("open fetch blob store")?;

    // Feed the provider's full address (from the ticket) to the endpoint's
    // address lookup so the downloader can dial it directly / via relay.
    let lookup = MemoryLookup::new();
    lookup.add_endpoint_info(ticket.addr().clone());
    let endpoint = Endpoint::builder(presets::N0)
        .relay_mode(RelayMode::Default)
        .address_lookup(lookup)
        .bind()
        .await
        .context("bind endpoint")?;

    println!("Waiting for the endpoint to come online (relay handshake)...");
    endpoint.online().await;

    let provider = ticket.addr().id;
    println!(
        "Downloading collection {} from {} into {} (resumes if re-run)...",
        ticket.hash(),
        provider.fmt_short(),
        dest.display()
    );
    let started = std::time::Instant::now();
    blobs::fetch_collection_to_dir(&store, &endpoint, provider, ticket.hash(), "pkg/validation", dest)
        .await
        .context("fetch collection")?;
    println!("Download complete in {:.1}s.", started.elapsed().as_secs_f64());

    // If the served directory was a package (has a manifest), verify every
    // payload's full-content xxh3 — hash-confirmed delivery.
    if dest.join(package::MANIFEST_FILENAME).exists() {
        print!("Verifying package against manifest... ");
        match package::validate_package(dest) {
            Ok(()) => println!("OK — every payload hash matches."),
            Err(e) => {
                println!("FAILED");
                return Err(e).context("package verification failed");
            }
        }
    } else {
        println!("No manifest.ndjson in the collection; skipping package verification.");
    }

    endpoint.close().await;
    store.shutdown().await.ok();
    println!("Done.");
    Ok(())
}
