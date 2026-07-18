//! Dial-outcome taxonomy for the sync send path (Task 3.1).
//!
//! The send engine retries package delivery forever (spec §2); every failed
//! serve/announce attempt records an unclassified `last_error` string. This
//! module gives that failure a stable, machine-readable *class* so a later UI
//! task can map it to human text and an operator can grep the structured log.
//!
//! The engine only ever sees an [`anyhow`] error chain through the
//! [`SharingTransport`](crate::sharing::SharingTransport) trait — never a typed
//! iroh error — so classification is purely string-based, generalizing the
//! connect-error classifier in `sharing::iroh::node` (`classify_connect_err`).
//! A class is a best-effort diagnostic hint, never an authorization signal.

/// Coarse cause of a failed serve/announce attempt, derived from its error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectClass {
    /// No addressing / no route to the peer — it looks unreachable at the
    /// network layer (no known addresses, network unreachable).
    NoRoute,
    /// A relay path is implicated in the failure — the direct route was absent
    /// and the relay leg could not carry the connection.
    RelayUnreachable,
    /// The peer is up but declined the connection (unauthorized / closed by
    /// peer / connection refused).
    Refused,
    /// The dial/announce timed out with no relay hint.
    Timeout,
    /// The local or remote endpoint is not started yet (loopback "peer not
    /// started" / "endpoint not started", or the same shape from the real node).
    NotStarted,
    /// Anything the string match could not place.
    Other,
}

impl ConnectClass {
    /// Stable snake_case tag for the `last_error` prefix and the `class` log
    /// field. Never changes once shipped — the UI maps these to human text.
    pub fn tag(self) -> &'static str {
        match self {
            ConnectClass::NoRoute => "no_route",
            ConnectClass::RelayUnreachable => "relay_unreachable",
            ConnectClass::Refused => "refused",
            ConnectClass::Timeout => "timeout",
            ConnectClass::NotStarted => "not_started",
            ConnectClass::Other => "other",
        }
    }
}

/// Classify a failed serve/announce error message into a [`ConnectClass`]. Pure
/// over the (lowercased) `anyhow`-chain text so it is unit-testable without a
/// network. Order matters: the most specific / least ambiguous signals are
/// tested first (a refusing peer that also times out is still *refused*; a relay
/// leg named alongside a timeout is *relay_unreachable*, not a plain timeout).
pub fn classify_send_error(msg: &str) -> ConnectClass {
    let m = msg.to_lowercase();
    if m.contains("not started") {
        // Loopback "peer not started" / "endpoint not started" and the real
        // node's pre-online shape: neither peer nor local endpoint is up yet.
        ConnectClass::NotStarted
    } else if m.contains("unauthorized")
        || m.contains("closed by peer")
        || m.contains("refused")
        || m.contains("forbidden")
    {
        ConnectClass::Refused
    } else if m.contains("relay") {
        // A relay leg named on the error path implicates the relay route — wins
        // over a co-occurring timeout (mirrors the reference's relay-hint rung).
        ConnectClass::RelayUnreachable
    } else if m.contains("timed out") || m.contains("timeout") {
        ConnectClass::Timeout
    } else if m.contains("no route")
        || m.contains("unreachable")
        || m.contains("no known address")
        || m.contains("no addressing")
    {
        ConnectClass::NoRoute
    } else {
        ConnectClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_stable_snake_case() {
        assert_eq!(ConnectClass::NoRoute.tag(), "no_route");
        assert_eq!(ConnectClass::RelayUnreachable.tag(), "relay_unreachable");
        assert_eq!(ConnectClass::Refused.tag(), "refused");
        assert_eq!(ConnectClass::Timeout.tag(), "timeout");
        assert_eq!(ConnectClass::NotStarted.tag(), "not_started");
        assert_eq!(ConnectClass::Other.tag(), "other");
    }

    #[test]
    fn classifies_representative_error_texts() {
        // (input, expected). The inputs mirror the real loopback / iroh chains
        // the engine actually sees via `format!("{e:#}")`.
        let cases: &[(&str, ConnectClass)] = &[
            // NotStarted — loopback + real-node "not started" shapes.
            ("peer not started: abcd1234", ConnectClass::NotStarted),
            ("endpoint not started", ConnectClass::NotStarted),
            (
                "announce package: peer not started: ff00",
                ConnectClass::NotStarted,
            ),
            ("Peer Not Started: FF00", ConnectClass::NotStarted),
            // Refused — the peer is up but declined us.
            ("closed by peer: unauthorized", ConnectClass::Refused),
            ("connection refused", ConnectClass::Refused),
            ("forbidden by connect gate", ConnectClass::Refused),
            // RelayUnreachable — a relay leg is named; wins over a co-occurring timeout.
            ("no relay connection; relay unreachable", ConnectClass::RelayUnreachable),
            ("dial timed out over relay us-east", ConnectClass::RelayUnreachable),
            // Timeout — timed out with no relay hint.
            ("connection timed out", ConnectClass::Timeout),
            ("handshake timeout", ConnectClass::Timeout),
            // NoRoute — no addressing / network unreachable.
            ("no route to host", ConnectClass::NoRoute),
            ("network is unreachable", ConnectClass::NoRoute),
            ("no known addresses for node", ConnectClass::NoRoute),
            // Other — unplaceable.
            ("serve package: disk quota exceeded", ConnectClass::Other),
        ];
        for (input, expected) in cases {
            assert_eq!(
                classify_send_error(input),
                *expected,
                "classify_send_error({input:?})"
            );
        }
    }
}
