//! Wire protocol for the sync control channel (custom ALPN `athenaeum/sync/1`).
//!
//! Message kinds flow over bidirectional QUIC streams between paired peers:
//! [`Msg::Announce`] (provider → receiver, "here is a fetchable package") and
//! [`Msg::Ack`] (receiver → provider, "here is what happened to each frame").
//! They are the on-wire counterpart of the in-process
//! [`TransportEvent`](crate::sharing::types::TransportEvent)s the loopback mock
//! delivers directly, so the sync engine (task A4) behaves identically over
//! either transport.
//!
//! The P2P dedup handshake adds three more: [`Msg::Offer`] (provider lists the
//! frames it could send, keyed by `rel_path` + sampling hash), [`Msg::Want`]
//! (receiver replies with the subset it actually wants plus the ambiguous
//! sampling-hash candidates it needs disambiguated), and [`Msg::FullHashes`]
//! (provider answers those candidates with full-file xxh3 digests). Their
//! payloads are [`OfferEntry`] / [`FullHashEntry`].
//!
//! Encoding is [postcard] — compact, `no_std`-friendly, and already the wire
//! format iroh itself uses for tickets. One `Msg` is one stream: the sender
//! writes the encoded bytes and finishes; the receiver reads to end, decodes,
//! and replies with a one-byte delivery ack (so the sender can close cleanly).
//!
//! [postcard]: https://docs.rs/postcard

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sharing::types::{FrameReceipt, PackageAnnounce, PackageId};

/// One frame the provider could send, as advertised in a [`Msg::Offer`].
///
/// `sampling_hash` is the cheap first/middle/last-512KB xxh3 digest; the pair
/// `(rel_path, sampling_hash)` is the offer key the receiver diffs against its
/// own catalog. `byte_size` lets the receiver spot a same-path/same-sample file
/// whose length differs (a guaranteed non-duplicate, no full hash needed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferEntry {
    pub rel_path: String,
    pub sampling_hash: String,
    pub byte_size: u64,
}

/// A provider's full-file digest for one offered frame, in a [`Msg::FullHashes`].
///
/// Sent only for the `rel_path`s the receiver flagged as sampling-hash
/// candidates in its [`Msg::Want`]: `xxh3_full` is the full-content xxh3 that
/// settles whether a same-sample file is truly identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullHashEntry {
    pub rel_path: String,
    pub sampling_hash: String,
    pub xxh3_full: String,
}

/// A single control message on the `athenaeum/sync/1` channel.
///
/// The `Announce`/`Ack` variants deliberately mirror the two peer-to-peer
/// [`TransportEvent`](crate::sharing::types::TransportEvent)s that carry data
/// between endpoints (`FetchProgress` is fetch-local UI data and never crosses
/// the wire). `Announce` carries a [`PackageAnnounce`] whose `root_hash` is the
/// iroh-blobs collection hash the receiver downloads by. The `Offer`/`Want`/
/// `FullHashes` variants form the P2P dedup handshake (keyed by `rel_path`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    /// Provider advertises a fetchable package to a receiver.
    Announce(PackageAnnounce),
    /// Receiver reports per-frame receipts for a package back to the provider.
    Ack {
        package_id: PackageId,
        receipts: Vec<FrameReceipt>,
    },
    /// Provider lists the frames it could send for a package (dedup handshake).
    Offer {
        package_id: PackageId,
        entries: Vec<OfferEntry>,
    },
    /// Receiver replies with the frames it wants plus the `rel_path`s whose
    /// sampling hash needs full-hash disambiguation (dedup handshake).
    Want {
        package_id: PackageId,
        want: Vec<String>,
        candidates: Vec<String>,
    },
    /// Provider answers `Want` candidates with full-file digests (dedup handshake).
    FullHashes {
        package_id: PackageId,
        entries: Vec<FullHashEntry>,
    },
}

impl Msg {
    /// Encode this message to its postcard byte representation.
    pub fn encode(&self) -> Result<Vec<u8>> {
        postcard::to_stdvec(self).context("postcard-encode sync control message")
    }

    /// Decode a message from postcard bytes read off the control stream.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).context("postcard-decode sync control message")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::types::{PackageId, ReceiptOutcome};

    fn sample_announce() -> PackageAnnounce {
        PackageAnnounce {
            package_id: PackageId("pkg-uuid-1".to_string()),
            root_hash: "blake3-collection-hash".to_string(),
            byte_size: 4096,
            frame_count: 3,
        }
    }

    #[test]
    fn announce_roundtrips_through_postcard() {
        let msg = Msg::Announce(sample_announce());
        let bytes = msg.encode().unwrap();
        let back = Msg::decode(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn ack_roundtrips_through_postcard() {
        let msg = Msg::Ack {
            package_id: PackageId("pkg-uuid-1".to_string()),
            receipts: vec![
                FrameReceipt {
                    frame_uuid: "f1".to_string(),
                    xxh3: "0011223344556677".to_string(),
                    outcome: ReceiptOutcome::Ingested,
                },
                FrameReceipt {
                    frame_uuid: "f2".to_string(),
                    xxh3: "8899aabbccddeeff".to_string(),
                    outcome: ReceiptOutcome::Rejected("bad hash".to_string()),
                },
            ],
        };
        let bytes = msg.encode().unwrap();
        let back = Msg::decode(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn decode_rejects_garbage() {
        // Truncated / non-postcard bytes must error, not panic.
        assert!(Msg::decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn offer_want_fullhashes_roundtrip() {
        let pid = PackageId("pkg-1".into());
        for msg in [
            Msg::Offer {
                package_id: pid.clone(),
                entries: vec![OfferEntry {
                    rel_path: "M31/L_0001.fits".into(),
                    sampling_hash: "00ff".into(),
                    byte_size: 12,
                }],
            },
            Msg::Want {
                package_id: pid.clone(),
                want: vec!["a".into()],
                candidates: vec!["b".into()],
            },
            Msg::FullHashes {
                package_id: pid.clone(),
                entries: vec![FullHashEntry {
                    rel_path: "b".into(),
                    sampling_hash: "00ff".into(),
                    xxh3_full: "1122334455667788".into(),
                }],
            },
        ] {
            let back = Msg::decode(&msg.encode().unwrap()).unwrap();
            assert_eq!(msg, back);
        }
    }
}
