//! Wire protocol for the sync control channel (custom ALPN `athenaeum/sync/1`).
//!
//! Two message kinds flow over bidirectional QUIC streams between paired peers:
//! [`Msg::Announce`] (provider → receiver, "here is a fetchable package") and
//! [`Msg::Ack`] (receiver → provider, "here is what happened to each frame").
//! They are the on-wire counterpart of the in-process
//! [`TransportEvent`](crate::sharing::types::TransportEvent)s the loopback mock
//! delivers directly, so the sync engine (task A4) behaves identically over
//! either transport.
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

/// A single control message on the `athenaeum/sync/1` channel.
///
/// The variants deliberately mirror the two peer-to-peer
/// [`TransportEvent`](crate::sharing::types::TransportEvent)s that carry data
/// between endpoints (`FetchProgress` is fetch-local UI data and never crosses
/// the wire). `Announce` carries a [`PackageAnnounce`] whose `root_hash` is the
/// iroh-blobs collection hash the receiver downloads by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    /// Provider advertises a fetchable package to a receiver.
    Announce(PackageAnnounce),
    /// Receiver reports per-frame receipts for a package back to the provider.
    Ack {
        package_id: PackageId,
        receipts: Vec<FrameReceipt>,
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
}
