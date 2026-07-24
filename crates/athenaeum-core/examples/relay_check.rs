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
//! It ALSO gates QUIC address discovery (QAD), which is what makes hole punching
//! possible at all: after the handshake it waits up to [`QAD_SETTLE`] for a
//! PUBLIC address to appear in `endpoint.addr()`. Only QAD (or a router port
//! mapping) can put one there, so "private addresses only" means the relay is not
//! answering QAD and every peer behind NAT is stuck relaying. The check exists
//! because that exact failure shipped unnoticed: the relays served QAD on UDP
//! 8443 while every client probes iroh's `DEFAULT_RELAY_QUIC_PORT` (7842), which
//! this example could not see because it only asserted the websocket handshake.
//! A QAD failure fails the run (exit != 0). Skip it with `--no-qad`.
//!
//! **Caveat:** run this from a machine behind NAT (the normal user situation). On
//! a host whose own NIC carries a public address the check passes trivially — the
//! public addr is then a local address, not a QAD discovery, and the public API
//! exposes no way to tell the two apart (`Endpoint::net_report` is feature-gated).
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
//!   cargo run -p athenaeum-core --example relay_check -- --no-qad
//!   cargo run -p athenaeum-core --example relay_check -- --paths \
//!       --expect-relay https://relay1.artfrom.space:8443
//!   cargo run -p athenaeum-core --example relay_check -- --sync-dir /path/sync \
//!       https://relay-ams.artfrom.space:8443

use std::net::IpAddr;
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

/// How long to wait, after the home relay is up, for a QAD-discovered public
/// address to land in `endpoint.addr()`. A working relay answers the first QAD
/// probe within a second or two (the probe itself times out at 3 s and is
/// retried), so this is generous headroom, not a tuning knob.
const QAD_SETTLE: Duration = Duration::from_secs(8);

/// Poll step while waiting for [`QAD_SETTLE`].
const QAD_POLL: Duration = Duration::from_millis(500);

/// Whether `ip` is unroutable from the public internet — RFC1918, CGNAT,
/// loopback/link-local, IPv6 ULA. A direct address that is NOT one of these can
/// only have come from QAD or a router port mapping (or a genuinely public NIC —
/// see the caveat in the module docs).
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT 100.64/10
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local
        }
    }
}

/// Per-OS default for the desktop app's sync dir (`<app-data>/sync`, where the
/// device key lives), mirroring Tauri's app-data resolution for our identifier:
/// macOS `~/Library/Application Support`, Windows `%APPDATA%` (Roaming), Linux
/// `$XDG_DATA_HOME` falling back to `~/.local/share`. `--sync-dir` overrides.
fn default_sync_dir() -> Result<std::path::PathBuf> {
    const IDENT: &str = "com.vsharifov.athenaeum";
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").context("HOME not set; pass --sync-dir")?;
        Ok(std::path::Path::new(&home)
            .join("Library/Application Support")
            .join(IDENT)
            .join("sync"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").context("APPDATA not set; pass --sync-dir")?;
        Ok(std::path::Path::new(&appdata).join(IDENT).join("sync"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = match std::env::var("XDG_DATA_HOME") {
            Ok(x) if !x.is_empty() => std::path::PathBuf::from(x),
            _ => {
                let home = std::env::var("HOME")
                    .context("XDG_DATA_HOME/HOME not set; pass --sync-dir")?;
                std::path::Path::new(&home).join(".local/share")
            }
        };
        Ok(base.join(IDENT).join("sync"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut ephemeral = false;
    let mut paths = false;
    let mut qad_gate = true;
    let mut expect_relay: Option<String> = None;
    let mut sync_dir: Option<std::path::PathBuf> = None;
    let mut relays: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ephemeral" => ephemeral = true,
            "--paths" => paths = true,
            "--no-qad" => qad_gate = false,
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
            None => default_sync_dir()?,
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

    // The QAD gate only makes sense for a key the relays actually accept — an
    // ephemeral run is expected to be refused before any probe matters.
    let qad_gate = qad_gate && !ephemeral;

    let mut failures = 0usize;
    let mut qad_failures = 0usize;
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

                // QAD gate: wait for a public address to show up. Re-read
                // `endpoint.addr()` each poll — the first net report can land
                // after `online()` returns.
                if qad_gate {
                    let mut public: Option<String>;
                    let deadline = Instant::now() + QAD_SETTLE;
                    loop {
                        public = endpoint
                            .addr()
                            .ip_addrs()
                            .find(|sa| !is_private(sa.ip()))
                            .map(|sa| sa.to_string());
                        if public.is_some() || Instant::now() >= deadline {
                            break;
                        }
                        tokio::time::sleep(QAD_POLL).await;
                    }
                    match &public {
                        Some(a) => println!("     qad OK   public addr {a}"),
                        None => {
                            qad_failures += 1;
                            println!(
                                "     qad FAIL no public address within {QAD_SETTLE:?} — \
                                 this relay is not answering QUIC address discovery \
                                 (check `enable_quic_addr_discovery` + that UDP/7842 is \
                                 open at the cloud edge AND host-side). Peers behind NAT \
                                 cannot hole-punch through it; everything falls back to relaying."
                            );
                        }
                    }
                }

                if paths {
                    // The node's live self-reported address (== what the H1 reporter
                    // publishes to the hub): home relay(s) + direct addrs.
                    let addr = endpoint.addr();
                    let reported_relays: Vec<String> =
                        addr.relay_urls().map(|u| u.to_string()).collect();
                    let direct_addrs: Vec<String> = addr
                        .ip_addrs()
                        .map(|a| {
                            let kind = if is_private(a.ip()) { "priv" } else { "PUBLIC" };
                            format!("{a} [{kind}]")
                        })
                        .collect();
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
        return Ok(());
    }
    if failures == 0 {
        println!("all {} relays accepted this device.", relays.len());
    }
    if qad_gate && qad_failures == 0 && failures == 0 {
        println!("all {} relays answer QUIC address discovery.", relays.len());
    }
    match (failures, qad_failures) {
        (0, 0) => Ok(()),
        (0, q) => bail!(
            "{q} relay(s) accepted this device but answer no QUIC address discovery — \
             hole punching is dead through them"
        ),
        (f, 0) => bail!("{f} relay(s) failed with the registered device key"),
        (f, q) => bail!("{f} relay(s) failed the handshake; {q} answer no QUIC address discovery"),
    }
}
