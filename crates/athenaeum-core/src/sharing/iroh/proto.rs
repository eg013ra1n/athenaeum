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

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::sharing::types::{
    FrameReceipt, NodeId, PackageAnnounce, PackageAnnounceV2, PackageAnnounceV3, PackageId,
    RevokeReason, TransportEvent,
};

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
/// between endpoints (fetch progress is fetch-local UI data delivered on the
/// [`FetchSink`](crate::sharing::FetchSink) callback and never crosses the wire).
/// `Announce` carries a [`PackageAnnounce`] whose `root_hash` is the
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
    // Slice-4 collab exchange — appended; postcard indices of the variants above
    // are frozen. Postcard encodes each variant by its DECLARATION INDEX (a
    // leading varint), so new variants may ONLY be appended at the END and no
    // existing variant/field may be reordered, retyped, or removed. Any such
    // BREAKING change is a wire-version bump: bump the `SYNC_ALPN` suffix
    // (`iroh/mod.rs`) so mismatched peers fail the handshake loudly instead of
    // silently mis-decoding, and update `sharing::wire_golden_tests` (the guard
    // that fails the build the moment these bytes drift — never silently re-pin).
    /// Provider advertises a PROJECT package (collab exchange, slice 4).
    /// `package_id` is the HUB package uuid (row key); `announce.package_id`
    /// stays the engine-minted wire id (ack correlation).
    ProjectAnnounce {
        project_id: String,
        package_id: String,
        announce: PackageAnnounce,
    },
    /// A receive-role member asks a holder to serve a project package (hub id).
    ProjectRequest {
        project_id: String,
        package_id: String,
    },
    // Transfers-status-v2 announce — appended AFTER `ProjectRequest`; the postcard
    // indices of every variant above stay frozen (same append-only rule as the
    // slice-4 block). `Announce2` is the v2 counterpart of `Announce`: it adds a
    // human batch name + the full file manifest so the receiver knows a package's
    // contents at announce time. The app sender emits only `Announce2`; the
    // receive side still decodes legacy `Announce` (v1) byte-for-byte.
    /// Provider advertises a fetchable package with its v2 manifest + batch name.
    Announce2(PackageAnnounceV2),
    // Transfers-batch-model announce + revoke (spec §D2) — appended AFTER
    // `Announce2` as the LAST two variants; every index above stays frozen (same
    // append-only rule as the blocks above). `Announce3` is the v3 counterpart of
    // `Announce2`: it adds the durable per-transfer `batch_uuid` so the receiver
    // keeps ONE row per transfer across re-attempts. The app sender emits only
    // `Announce3`; the receive side still decodes legacy `Announce` (v1) and
    // `Announce2` (v2) byte-for-byte, falling back to the wire `package_id` as the
    // batch key. `Revoke` is a one-shot, best-effort sender→receiver control
    // signal to abort an outstanding announce.
    /// Provider advertises a fetchable package with its v3 batch identity.
    Announce3(PackageAnnounceV3),
    /// Sender revokes an outstanding announce (abort the pending/in-flight transfer).
    Revoke {
        package_id: PackageId,
        reason: RevokeReason,
    },
}

/// Map a decoded *announce* control message — `Announce` (v1), `Announce2` (v2),
/// or `Announce3` (v3) — to its in-process
/// [`AnnounceReceived`](TransportEvent::AnnounceReceived) event, applying the
/// batch-identity migration fallback (spec §D2 Migration): v3 carries
/// `batch_uuid` on the wire; a legacy v1/v2 announce predates it, so the receiver
/// adopts the wire `package_id` as the `batch_uuid` (guaranteeing ONE stable
/// per-transfer key on every path). Shared by the transport accept loop and its
/// wire-golden fallback test so the mapping cannot drift.
///
/// # Panics
/// Only ever called on an announce variant (the accept loop guards the call);
/// any other `Msg` is a caller bug and hits `unreachable!`.
pub(crate) fn announce_received_from_msg(from: NodeId, msg: Msg) -> TransportEvent {
    match msg {
        // v1 announce (e.g. Perseus beta.3): no manifest extras; batch key falls
        // back to the wire package id.
        Msg::Announce(announce) => {
            let batch_uuid = announce.package_id.0.clone();
            TransportEvent::AnnounceReceived {
                from,
                announce,
                batch_name: None,
                batch_uuid,
                files: None,
            }
        }
        // v2 announce: split the manifest extras off the wire struct; batch key
        // still falls back to the wire package id (v2 predates batch identity).
        Msg::Announce2(v2) => {
            let PackageAnnounceV2 {
                package_id,
                root_hash,
                byte_size,
                frame_count,
                batch_name,
                files,
            } = v2;
            let batch_uuid = package_id.0.clone();
            TransportEvent::AnnounceReceived {
                from,
                announce: PackageAnnounce {
                    package_id,
                    root_hash,
                    byte_size,
                    frame_count,
                },
                batch_name: Some(batch_name),
                batch_uuid,
                files: Some(files),
            }
        }
        // v3 announce: the durable `batch_uuid` rides the wire — use it verbatim.
        Msg::Announce3(v3) => {
            let PackageAnnounceV3 {
                package_id,
                root_hash,
                byte_size,
                frame_count,
                batch_name,
                batch_uuid,
                files,
            } = v3;
            TransportEvent::AnnounceReceived {
                from,
                announce: PackageAnnounce {
                    package_id,
                    root_hash,
                    byte_size,
                    frame_count,
                },
                batch_name: Some(batch_name),
                batch_uuid,
                files: Some(files),
            }
        }
        other => unreachable!("announce_received_from_msg called with non-announce Msg: {other:?}"),
    }
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

/// Build the [`FullHashEntry`] list for the second dedup round, shared by both
/// transports so their handshake logic can't drift.
///
/// For each candidate `rel_path`, pair its offered `sampling_hash` (looked up in
/// `offer`) with the sender's own full-file xxh3 from `full_by_rel`. A candidate
/// the sender can't supply a full hash for is inserted straight into `wanted`
/// (the safe direction — never silently drop a frame) and omitted from the
/// query. The returned entries are what the sender puts in its [`Msg::FullHashes`].
pub(crate) fn build_full_hash_entries(
    offer: &[OfferEntry],
    candidates: &[String],
    full_by_rel: &HashMap<String, String>,
    wanted: &mut HashSet<String>,
) -> Vec<FullHashEntry> {
    let sampling_by_rel: HashMap<&str, &str> = offer
        .iter()
        .map(|e| (e.rel_path.as_str(), e.sampling_hash.as_str()))
        .collect();
    let mut entries = Vec::with_capacity(candidates.len());
    for rel in candidates {
        match full_by_rel.get(rel) {
            Some(full) => entries.push(FullHashEntry {
                rel_path: rel.clone(),
                sampling_hash: sampling_by_rel
                    .get(rel.as_str())
                    .copied()
                    .unwrap_or("")
                    .to_string(),
                xxh3_full: full.clone(),
            }),
            None => {
                tracing::warn!(rel_path = %rel, "negotiate_want candidate missing full hash; keeping wanted");
                wanted.insert(rel.clone());
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::types::{AnnounceFileEntry, PackageId, ReceiptOutcome};

    fn sample_announce() -> PackageAnnounce {
        PackageAnnounce {
            package_id: PackageId("pkg-uuid-1".to_string()),
            root_hash: "blake3-collection-hash".to_string(),
            byte_size: 4096,
            frame_count: 3,
        }
    }

    fn sample_announce_v2() -> PackageAnnounceV2 {
        PackageAnnounceV2 {
            package_id: PackageId("pkg-uuid-1".to_string()),
            root_hash: "blake3-collection-hash".to_string(),
            byte_size: 4096,
            frame_count: 3,
            batch_name: "Туманность M31".to_string(),
            files: vec![
                AnnounceFileEntry {
                    rel_path: "M31/L_0001.fits".to_string(),
                    byte_size: 4096,
                    frame_uuid: "frame-uuid-1".to_string(),
                },
                AnnounceFileEntry {
                    rel_path: "M31/L_0002.fits".to_string(),
                    byte_size: 2048,
                    frame_uuid: "frame-uuid-2".to_string(),
                },
            ],
        }
    }

    fn sample_announce_v3() -> PackageAnnounceV3 {
        PackageAnnounceV3 {
            package_id: PackageId("pkg-uuid-1".to_string()),
            root_hash: "blake3-collection-hash".to_string(),
            byte_size: 4096,
            frame_count: 3,
            batch_name: "Туманность M31".to_string(),
            batch_uuid: "batch-uuid-9".to_string(),
            files: vec![
                AnnounceFileEntry {
                    rel_path: "M31/L_0001.fits".to_string(),
                    byte_size: 4096,
                    frame_uuid: "frame-uuid-1".to_string(),
                },
                AnnounceFileEntry {
                    rel_path: "M31/L_0002.fits".to_string(),
                    byte_size: 2048,
                    frame_uuid: "frame-uuid-2".to_string(),
                },
            ],
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
    fn announce2_roundtrips_through_postcard() {
        let msg = Msg::Announce2(sample_announce_v2());
        let bytes = msg.encode().unwrap();
        let back = Msg::decode(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn package_announce_v2_roundtrips_through_postcard() {
        // The embedded struct (and its `Vec<AnnounceFileEntry>`) roundtrips on its
        // own, including the non-ASCII batch name and an empty manifest.
        for v2 in [
            sample_announce_v2(),
            PackageAnnounceV2 {
                files: Vec::new(),
                batch_name: String::new(),
                ..sample_announce_v2()
            },
        ] {
            let bytes = postcard::to_stdvec(&v2).unwrap();
            let back: PackageAnnounceV2 = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(v2, back);
        }
    }

    #[test]
    fn announce3_roundtrips_through_postcard() {
        let msg = Msg::Announce3(sample_announce_v3());
        let bytes = msg.encode().unwrap();
        let back = Msg::decode(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn package_announce_v3_roundtrips_through_postcard() {
        // The embedded struct roundtrips on its own, including the non-ASCII batch
        // name, an empty manifest, and an empty batch_uuid.
        for v3 in [
            sample_announce_v3(),
            PackageAnnounceV3 {
                files: Vec::new(),
                batch_name: String::new(),
                batch_uuid: String::new(),
                ..sample_announce_v3()
            },
        ] {
            let bytes = postcard::to_stdvec(&v3).unwrap();
            let back: PackageAnnounceV3 = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(v3, back);
        }
    }

    #[test]
    fn revoke_roundtrips_through_postcard_for_every_reason() {
        for reason in [
            RevokeReason::Cancelled,
            RevokeReason::Superseded,
            RevokeReason::Failed,
        ] {
            let msg = Msg::Revoke {
                package_id: PackageId("pkg-uuid-1".to_string()),
                reason,
            };
            let back = Msg::decode(&msg.encode().unwrap()).unwrap();
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn announce_received_from_msg_maps_every_version() {
        let from: NodeId = [7u8; 32];

        // v3 carries the durable batch_uuid on the wire — use it verbatim.
        match announce_received_from_msg(from, Msg::Announce3(sample_announce_v3())) {
            TransportEvent::AnnounceReceived {
                batch_uuid,
                batch_name,
                files,
                announce,
                ..
            } => {
                assert_eq!(batch_uuid, "batch-uuid-9");
                assert_eq!(batch_name.as_deref(), Some("Туманность M31"));
                assert!(files.is_some());
                assert_eq!(announce.package_id, PackageId("pkg-uuid-1".to_string()));
            }
            other => panic!("expected AnnounceReceived, got {other:?}"),
        }

        // v2 predates batch identity — batch_uuid falls back to the wire package id.
        match announce_received_from_msg(from, Msg::Announce2(sample_announce_v2())) {
            TransportEvent::AnnounceReceived {
                batch_uuid,
                batch_name,
                ..
            } => {
                assert_eq!(batch_uuid, "pkg-uuid-1");
                assert_eq!(batch_name.as_deref(), Some("Туманность M31"));
            }
            other => panic!("expected AnnounceReceived, got {other:?}"),
        }

        // v1 predates the extras entirely — no batch_name/files, batch_uuid == wire id.
        match announce_received_from_msg(from, Msg::Announce(sample_announce())) {
            TransportEvent::AnnounceReceived {
                batch_uuid,
                batch_name,
                files,
                ..
            } => {
                assert_eq!(batch_uuid, "pkg-uuid-1");
                assert!(batch_name.is_none());
                assert!(files.is_none());
            }
            other => panic!("expected AnnounceReceived, got {other:?}"),
        }
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

    #[test]
    fn project_announce_and_request_roundtrip() {
        for msg in [
            Msg::ProjectAnnounce {
                project_id: "p-1".into(),
                package_id: "hub-pkg-1".into(),
                announce: sample_announce(),
            },
            Msg::ProjectRequest {
                project_id: "p-1".into(),
                package_id: "hub-pkg-1".into(),
            },
        ] {
            let back = Msg::decode(&msg.encode().unwrap()).unwrap();
            assert_eq!(msg, back);
        }
    }
}
