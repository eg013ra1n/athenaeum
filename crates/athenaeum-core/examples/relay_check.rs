//! Relay connectivity check against the self-hosted iroh relays.
//!
//! For each relay URL this binds a REAL iroh endpoint (`RelayMode::Custom`
//! with that single relay) and waits for the home-relay handshake — the same
//! handshake the app's sync transport performs at startup, including the
//! relay's hub access-control callback. So it proves, per relay: DNS + TLS +
//! the relay websocket + the hub `relay-auth` decision for the presented key.
//!
//! By default it presents the machine's registered device identity
//! (`<sync-dir>/device_key` — the exact key the app binds from), so a healthy,
//! authorized relay reports `OK`. With `--ephemeral` it presents a fresh
//! unregistered key instead: every access-controlled relay is then EXPECTED to
//! refuse it (reported as a timeout) — that failure is the auth gate working.
//!
//! NOTE: binding the device key while the app is running briefly duplicates
//! the node id on whichever relay the app is homed on; the relay bumps the
//! older connection and the app re-establishes it automatically. Prefer
//! running this with the app closed.
//!
//! With `--paths` it additionally prints, per relay, the node's live self-reported
//! addresses after the handshake — its home relay(s) + direct addrs, read straight
//! off `endpoint.addr()`, which is exactly what the H1 reporter (Task 7) publishes
//! to the hub. `--expect-relay <url>` compares the reported home-relay set against
//! an expected url and prints MATCH / MISMATCH — the reported-vs-actual home-relay
//! check without wiring a hub token into this self-contained example (the honest
//! minimal form: the endpoint's own live address IS the actual, so no hub round
//! trip is needed to see a drift).
//!
//! Usage:
//!   cargo run -p athenaeum-core --example relay_check
//!   cargo run -p athenaeum-core --example relay_check -- --ephemeral
//!   cargo run -p athenaeum-core --example relay_check -- --paths
//!   cargo run -p athenaeum-core --example relay_check -- --paths \
//!       --expect-relay https://relay1.artfrom.space:8443
//!   cargo run -p athenaeum-core --example relay_check -- --sync-dir /path/sync \
//!       https://relay-ams.artfrom.space:8443

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use athenaeum_core::account::keys::{device_key_path, DeviceKey};
use iroh::endpoint::presets;
use iroh::{Endpoint, RelayMap, RelayMode};

const DEFAULT_RELAYS: [&str; 5] = [
    "https://relay1.artfrom.space:8443",
    "https://relay2.artfrom.space:8443",
    "https://relay-ru.artfrom.space:8443",
    "https://relay-ams.artfrom.space:8443",
    "https://test-relay.artfrom.space:8443",
];

const ONLINE_TIMEOUT: Duration = Duration::from_secs(12);

#[tokio::main]
async fn main() -> Result<()> {
    let mut ephemeral = false;
    let mut paths = false;
    let mut expect_relay: Option<String> = None;
    let mut sync_dir: Option<std::path::PathBuf> = None;
    let mut relays: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ephemeral" => ephemeral = true,
            "--paths" => paths = true,
            "--expect-relay" => {
                expect_relay = Some(args.next().context("--expect-relay needs a url")?);
            }
            "--sync-dir" => {
                let v = args.next().context("--sync-dir needs a path")?;
                sync_dir = Some(v.into());
            }
            url if url.starts_with("https://") => relays.push(url.to_string()),
            other => bail!("unknown argument: {other}"),
        }
    }
    // `--expect-relay` only makes sense alongside the `--paths` report.
    if expect_relay.is_some() {
        paths = true;
    }
    if relays.is_empty() {
        relays = DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect();
    }

    let key = if ephemeral {
        let dir = std::env::temp_dir().join(format!("relay-check-{}", std::process::id()));
        let key = DeviceKey::load_or_create_in(&dir)?;
        println!("identity : EPHEMERAL {} (unregistered — refusals expected)", key.pubkey_base64());
        key
    } else {
        let dir = match sync_dir {
            Some(d) => d,
            None => {
                let home = std::env::var("HOME").context("HOME not set; pass --sync-dir")?;
                std::path::Path::new(&home)
                    .join("Library/Application Support/com.vsharifov.athenaeum/sync")
            }
        };
        let path = device_key_path(&dir);
        if !path.exists() {
            bail!(
                "no device key at {} — pass --sync-dir pointing at the app's sync dir, \
                 or --ephemeral for an unregistered probe",
                path.display()
            );
        }
        let key = DeviceKey::load_or_create(&path)?;
        println!("identity : device key {} ({})", key.pubkey_base64(), path.display());
        key
    };
    println!();

    let mut failures = 0usize;
    for url in &relays {
        let map = RelayMap::try_from_iter([url.as_str()])
            .with_context(|| format!("invalid relay url {url}"))?;
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(key.secret_key())
            .relay_mode(RelayMode::Custom(map))
            .bind()
            .await
            .with_context(|| format!("bind endpoint for {url}"))?;

        let started = Instant::now();
        match tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online()).await {
            Ok(()) => {
                let addr = endpoint.addr();
                let home = addr
                    .relay_urls()
                    .next()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "<none>".into());
                println!("OK   {url}  home_relay={home}  in {:?}", started.elapsed());
                if paths {
                    // The node's live self-reported address (== what the H1 reporter
                    // publishes to the hub): home relay(s) + direct addrs.
                    let reported_relays: Vec<String> =
                        addr.relay_urls().map(|u| u.to_string()).collect();
                    let direct_addrs: Vec<String> =
                        addr.ip_addrs().map(|a| a.to_string()).collect();
                    println!("     reported home relays : {reported_relays:?}");
                    println!("     reported direct addrs: {direct_addrs:?}");
                    if let Some(exp) = &expect_relay {
                        let matched = reported_relays.iter().any(|r| r == exp);
                        println!(
                            "     expect-relay {exp} : {}",
                            if matched { "MATCH" } else { "MISMATCH" }
                        );
                    }
                }
            }
            Err(_) => {
                failures += 1;
                println!(
                    "FAIL {url}  no home relay within {ONLINE_TIMEOUT:?} \
                     (unreachable, or the hub access callback refused this key)"
                );
            }
        }
        endpoint.close().await;
    }

    println!();
    if ephemeral {
        println!("ephemeral run: FAILs above mean the access-control gate is working.");
        Ok(())
    } else if failures == 0 {
        println!("all {} relays accepted this device.", relays.len());
        Ok(())
    } else {
        bail!("{failures} relay(s) failed with the registered device key");
    }
}
