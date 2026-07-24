//! Transport telemetry: how much of our traffic rode a relay, and which path a
//! given peer is reachable on.
//!
//! Two questions the app could not answer before. The first — "what share of our
//! bytes goes through the relay?" — decides whether self-hosted relays stay cheap
//! and whether hole punching is actually working in the field; it went unanswered
//! through a months-long outage in which EVERY cross-NAT transfer was relayed
//! (the relays served QUIC address discovery on a port no client probes, so no
//! endpoint behind NAT ever learned its public address). The second — "did THIS
//! transfer go direct?" — is invisible from our own logs, because the bulk of a
//! transfer rides `iroh-blobs`' own downloader/connection pool and we never hold
//! that [`Connection`](iroh::endpoint::Connection); only the control channel is
//! instrumented ([`super::spawn_conn_path_diagnostics`]).
//!
//! Both are answered from public iroh APIs, with no per-connection plumbing:
//! [`Endpoint::metrics`] for the process-wide byte split and
//! [`Endpoint::remote_info`] for a per-peer path snapshot.
//!
//! NOTE: [`Endpoint::metrics`] is gated on iroh's `metrics` feature, which is one
//! of its DEFAULT features and is what this crate depends on. Setting
//! `default-features = false` on the `iroh` dependency would stop this module
//! compiling — that is deliberate: the fix is to re-enable the feature, not to
//! silently lose the numbers behind a local cfg.

use iroh::endpoint::TransportAddrUsage;
use iroh::{Endpoint, EndpointId};

use crate::sharing::types::NodeId;

/// Cumulative transport counters read off the endpoint. Absolute values since
/// bind; [`delta`](Self::delta) turns two snapshots into the traffic of a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TransportCounters {
    /// Bytes sent over a relay.
    pub send_relay_bytes: u64,
    /// Bytes sent over a direct IP path (v4 + v6).
    pub send_direct_bytes: u64,
    /// Data bytes received over a relay.
    pub recv_relay_bytes: u64,
    /// Data bytes received over a direct IP path (v4 + v6).
    pub recv_direct_bytes: u64,
    /// Direct network paths opened to peers.
    pub paths_direct: u64,
    /// Relayed network paths opened to peers.
    pub paths_relay: u64,
    /// Hole-punching attempts initiated (client side only, per iroh).
    pub holepunch_attempts: u64,
    /// Home-relay changes, including the initial assignment. A steadily climbing
    /// value is relay ping-pong, not healthy re-homing.
    pub relay_home_change: u64,
    /// Connections that ended up direct.
    pub conns_direct: u64,
    /// Connections opened (completed the TLS handshake).
    pub conns_opened: u64,
}

impl TransportCounters {
    /// Read the endpoint's current counters.
    pub(crate) fn snapshot(endpoint: &Endpoint) -> Self {
        let m = &endpoint.metrics().socket;
        Self {
            send_relay_bytes: m.send_relay.get(),
            send_direct_bytes: m.send_ipv4.get().saturating_add(m.send_ipv6.get()),
            recv_relay_bytes: m.recv_data_relay.get(),
            recv_direct_bytes: m
                .recv_data_ipv4
                .get()
                .saturating_add(m.recv_data_ipv6.get()),
            paths_direct: m.paths_direct.get(),
            paths_relay: m.paths_relay.get(),
            holepunch_attempts: m.holepunch_attempts.get(),
            relay_home_change: m.relay_home_change.get(),
            conns_direct: m.num_conns_direct.get(),
            conns_opened: m.num_conns_opened.get(),
        }
    }

    /// Traffic between `earlier` and `self`. Saturating: a counter can only be
    /// reset by a rebind, which also resets the baseline the caller holds, so an
    /// apparent decrease is reported as zero rather than wrapping.
    pub(crate) fn delta(&self, earlier: &Self) -> Self {
        Self {
            send_relay_bytes: self
                .send_relay_bytes
                .saturating_sub(earlier.send_relay_bytes),
            send_direct_bytes: self
                .send_direct_bytes
                .saturating_sub(earlier.send_direct_bytes),
            recv_relay_bytes: self
                .recv_relay_bytes
                .saturating_sub(earlier.recv_relay_bytes),
            recv_direct_bytes: self
                .recv_direct_bytes
                .saturating_sub(earlier.recv_direct_bytes),
            paths_direct: self.paths_direct.saturating_sub(earlier.paths_direct),
            paths_relay: self.paths_relay.saturating_sub(earlier.paths_relay),
            holepunch_attempts: self
                .holepunch_attempts
                .saturating_sub(earlier.holepunch_attempts),
            relay_home_change: self
                .relay_home_change
                .saturating_sub(earlier.relay_home_change),
            conns_direct: self.conns_direct.saturating_sub(earlier.conns_direct),
            conns_opened: self.conns_opened.saturating_sub(earlier.conns_opened),
        }
    }

    /// Whether this (delta) window moved no bytes and opened no path — an idle
    /// app must not write a log line every tick.
    pub(crate) fn is_quiet(&self) -> bool {
        self.send_relay_bytes == 0
            && self.send_direct_bytes == 0
            && self.recv_relay_bytes == 0
            && self.recv_direct_bytes == 0
            && self.paths_direct == 0
            && self.paths_relay == 0
            && self.relay_home_change == 0
    }

    /// Emit this window at `info!`. `window` names what the numbers cover
    /// (`"tick"` / `"session"`), so a reader can tell a 5-minute slice from the
    /// whole run.
    pub(crate) fn log(&self, window: &'static str) {
        tracing::info!(
            window,
            send_relay_bytes = self.send_relay_bytes,
            send_direct_bytes = self.send_direct_bytes,
            recv_relay_bytes = self.recv_relay_bytes,
            recv_direct_bytes = self.recv_direct_bytes,
            paths_direct = self.paths_direct,
            paths_relay = self.paths_relay,
            holepunch_attempts = self.holepunch_attempts,
            relay_home_change = self.relay_home_change,
            conns_direct = self.conns_direct,
            conns_opened = self.conns_opened,
            "transport traffic"
        );
    }
}

/// The kind of one network path to a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    Direct,
    Relay,
    Other,
}

/// Classify the ACTIVE paths to one peer into the `conn_path` log value:
/// `direct` (only IP paths in use), `relay` (only relayed), `mixed` (both — a
/// hole punch that has landed but not yet displaced the relay), `unknown` (the
/// peer is not in the endpoint's remote map, or has no active path right now).
pub(crate) fn classify_paths(paths: impl IntoIterator<Item = PathKind>) -> &'static str {
    let (mut direct, mut relay, mut other) = (false, false, false);
    for p in paths {
        match p {
            PathKind::Direct => direct = true,
            PathKind::Relay => relay = true,
            PathKind::Other => other = true,
        }
    }
    match (direct, relay, other) {
        (true, true, _) => "mixed",
        (true, false, _) => "direct",
        (false, true, _) => "relay",
        (false, false, true) => "other",
        (false, false, false) => "unknown",
    }
}

/// `conn_path` for `peer`: how it is reachable RIGHT NOW, per the endpoint's own
/// remote map. Best-effort diagnostics — `unknown` when iroh has already dropped
/// the peer's state (it keeps it only for a while after last use) or the id is
/// unparseable. Never fails, never blocks a transfer.
pub(crate) async fn peer_conn_path(endpoint: &Endpoint, peer: NodeId) -> &'static str {
    let Ok(id) = EndpointId::from_bytes(&peer) else {
        return "unknown";
    };
    let Some(info) = endpoint.remote_info(id).await else {
        return "unknown";
    };
    classify_paths(info.addrs().filter_map(|a| {
        if !matches!(a.usage(), TransportAddrUsage::Active) {
            return None;
        }
        let addr = a.addr();
        Some(if addr.is_ip() {
            PathKind::Direct
        } else if addr.is_relay() {
            PathKind::Relay
        } else {
            PathKind::Other
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(send_relay: u64, send_direct: u64) -> TransportCounters {
        TransportCounters {
            send_relay_bytes: send_relay,
            send_direct_bytes: send_direct,
            ..Default::default()
        }
    }

    #[test]
    fn delta_subtracts_the_earlier_snapshot() {
        let d = counters(300, 1000).delta(&counters(100, 400));
        assert_eq!(d.send_relay_bytes, 200);
        assert_eq!(d.send_direct_bytes, 600);
    }

    #[test]
    fn delta_of_a_reset_counter_saturates_to_zero() {
        // A rebind resets iroh's counters; report nothing rather than wrap.
        let d = counters(5, 5).delta(&counters(100, 100));
        assert_eq!(d.send_relay_bytes, 0);
        assert_eq!(d.send_direct_bytes, 0);
    }

    #[test]
    fn an_idle_window_is_quiet_but_a_relay_rehome_is_not() {
        assert!(TransportCounters::default().is_quiet());
        assert!(!counters(1, 0).is_quiet());
        let rehomed = TransportCounters {
            relay_home_change: 1,
            ..Default::default()
        };
        assert!(
            !rehomed.is_quiet(),
            "relay ping-pong must surface even with no traffic"
        );
    }

    #[test]
    fn classify_paths_covers_every_combination() {
        use PathKind::*;
        assert_eq!(classify_paths([Direct]), "direct");
        assert_eq!(classify_paths([Relay]), "relay");
        assert_eq!(classify_paths([Relay, Direct]), "mixed");
        assert_eq!(classify_paths([Other]), "other");
        assert_eq!(classify_paths([]), "unknown");
    }
}
