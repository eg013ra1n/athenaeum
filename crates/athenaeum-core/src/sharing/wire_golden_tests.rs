//! Golden-byte pins for the postcard control-wire format (audit TECH-1-lite).
//!
//! The sync control channel (`SYNC_ALPN`, `sharing/iroh/mod.rs`) speaks postcard
//! with POSITIONAL encoding: enum variants are keyed by their declaration index
//! (a leading varint), struct fields by their order — there are NO field names
//! on the wire. That makes every one of these types silently fragile: reorder a
//! `Msg` variant, insert a field, or renumber `ReceiptOutcome` and OLD peers
//! decode the new bytes into the WRONG variant/field with no error — the failure
//! surfaces only as an undiagnosable stalled transfer on the far side.
//!
//! These tests freeze the byte image of one fixed sample per wire type against a
//! literal captured from HEAD. They do NOT validate that the current bytes are
//! "correct" — their sole job is to FAIL LOUDLY the moment the encoding drifts,
//! forcing a deliberate decision at the seam instead of a silent break.
//!
//! ── THE RULE (read before you touch a failing assertion) ─────────────────────
//! A failing assertion here means you have made a BREAKING WIRE CHANGE. You MUST:
//!   1. Bump the `SYNC_ALPN` suffix (`sharing/iroh/mod.rs`): `athenaeum/sync/1`
//!      → `athenaeum/sync/2`. The ALPN is the handshake's version signal — an
//!      old and a new peer then FAIL TO CONNECT loudly instead of decode-dying
//!      silently against a positional mismatch.
//!   2. Add a NEW golden set for the new format (keep the old one if any code
//!      still parses legacy bytes).
//! NEVER silently re-pin a literal to make the test pass — that is exactly the
//! silent-drift class this guard exists to stop.
//!
//! Scope: the postcard control wire only — the `Msg` envelope (`proto.rs`) and
//! every type it embeds (`PackageAnnounce`, `PackageId`, `FrameReceipt` with all
//! four `ReceiptOutcome` variants, `OfferEntry`, `FullHashEntry`). `TransportEvent`
//! / `StartInfo` are in-process only (no serde, never serialized). The package
//! manifest is JSON + `MANIFEST_VERSION`-versioned (field-named, not positional),
//! so it is not part of this positional-fragility guard.

use crate::sharing::iroh::proto::{FullHashEntry, Msg, OfferEntry};
use crate::sharing::types::{FrameReceipt, PackageAnnounce, PackageId, ReceiptOutcome};

/// Render bytes as lowercase hex, matching the pinned-literal format below.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Fixed sample values ──────────────────────────────────────────────────────
// NEVER change these once pinned; add a NEW sample + golden set on a wire bump.

fn sample_announce() -> PackageAnnounce {
    PackageAnnounce {
        package_id: PackageId("pkg-uuid-1".to_string()),
        root_hash: "blake3-collection-hash".to_string(),
        byte_size: 4096,
        frame_count: 3,
    }
}

fn sample_receipt(outcome: ReceiptOutcome) -> FrameReceipt {
    FrameReceipt {
        frame_uuid: "frame-uuid-1".to_string(),
        xxh3: "0011223344556677".to_string(),
        outcome,
    }
}

fn sample_offer_entry() -> OfferEntry {
    OfferEntry {
        rel_path: "M31/L_0001.fits".to_string(),
        sampling_hash: "00ff".to_string(),
        byte_size: 12,
    }
}

fn sample_full_hash_entry() -> FullHashEntry {
    FullHashEntry {
        rel_path: "M31/L_0001.fits".to_string(),
        sampling_hash: "00ff".to_string(),
        xxh3_full: "1122334455667788".to_string(),
    }
}

/// Every serialized wire type, paired with its Wire-frozen golden hex captured
/// from HEAD (`__wire_golden_generate`). See THE RULE at the top of this file:
/// a mismatch is a BREAKING wire change ⇒ bump `SYNC_ALPN` + add a new golden
/// set; never silently re-pin.
fn golden_cases() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    let pid = PackageId("pkg-uuid-1".to_string());
    vec![
        // ── Embedded types ──────────────────────────────────────────────────
        (
            "package_id",
            postcard::to_stdvec(&pid).unwrap(),
            "0a706b672d757569642d31",
        ),
        (
            "package_announce",
            postcard::to_stdvec(&sample_announce()).unwrap(),
            "0a706b672d757569642d3116626c616b65332d636f6c6c656374696f6e2d68617368802003",
        ),
        // FrameReceipt × each ReceiptOutcome variant — the trailing discriminant
        // byte (00/01/02/03) is what freezes the outcome variant ORDER.
        (
            "receipt_ingested",
            postcard::to_stdvec(&sample_receipt(ReceiptOutcome::Ingested)).unwrap(),
            "0c6672616d652d757569642d31103030313132323333343435353636373700",
        ),
        (
            "receipt_duplicate",
            postcard::to_stdvec(&sample_receipt(ReceiptOutcome::Duplicate)).unwrap(),
            "0c6672616d652d757569642d31103030313132323333343435353636373701",
        ),
        (
            "receipt_rejected",
            postcard::to_stdvec(&sample_receipt(ReceiptOutcome::Rejected("bad hash".to_string())))
                .unwrap(),
            "0c6672616d652d757569642d31103030313132323333343435353636373702086261642068617368",
        ),
        (
            "receipt_cancelled",
            postcard::to_stdvec(&sample_receipt(ReceiptOutcome::Cancelled)).unwrap(),
            "0c6672616d652d757569642d31103030313132323333343435353636373703",
        ),
        (
            "offer_entry",
            postcard::to_stdvec(&sample_offer_entry()).unwrap(),
            "0f4d33312f4c5f303030312e6669747304303066660c",
        ),
        (
            "full_hash_entry",
            postcard::to_stdvec(&sample_full_hash_entry()).unwrap(),
            "0f4d33312f4c5f303030312e6669747304303066661031313232333334343535363637373838",
        ),
        // ── Msg envelope × every variant ────────────────────────────────────
        // The LEADING discriminant byte (00..=06) freezes the Msg variant ORDER.
        (
            "msg_announce",
            Msg::Announce(sample_announce()).encode().unwrap(),
            "000a706b672d757569642d3116626c616b65332d636f6c6c656374696f6e2d68617368802003",
        ),
        (
            "msg_ack",
            Msg::Ack {
                package_id: pid.clone(),
                receipts: vec![
                    sample_receipt(ReceiptOutcome::Ingested),
                    sample_receipt(ReceiptOutcome::Duplicate),
                    sample_receipt(ReceiptOutcome::Rejected("bad hash".to_string())),
                    sample_receipt(ReceiptOutcome::Cancelled),
                ],
            }
            .encode()
            .unwrap(),
            "010a706b672d757569642d31040c6672616d652d757569642d311030303131323233333434353536363737000c6672616d652d757569642d311030303131323233333434353536363737010c6672616d652d757569642d311030303131323233333434353536363737020862616420686173680c6672616d652d757569642d31103030313132323333343435353636373703",
        ),
        (
            "msg_offer",
            Msg::Offer {
                package_id: pid.clone(),
                entries: vec![sample_offer_entry()],
            }
            .encode()
            .unwrap(),
            "020a706b672d757569642d31010f4d33312f4c5f303030312e6669747304303066660c",
        ),
        (
            "msg_want",
            Msg::Want {
                package_id: pid.clone(),
                want: vec!["a".to_string()],
                candidates: vec!["b".to_string()],
            }
            .encode()
            .unwrap(),
            "030a706b672d757569642d31010161010162",
        ),
        (
            "msg_full_hashes",
            Msg::FullHashes {
                package_id: pid.clone(),
                entries: vec![sample_full_hash_entry()],
            }
            .encode()
            .unwrap(),
            "040a706b672d757569642d31010f4d33312f4c5f303030312e6669747304303066661031313232333334343535363637373838",
        ),
        (
            "msg_project_announce",
            Msg::ProjectAnnounce {
                project_id: "proj-1".to_string(),
                package_id: "hub-pkg-1".to_string(),
                announce: sample_announce(),
            }
            .encode()
            .unwrap(),
            "050670726f6a2d31096875622d706b672d310a706b672d757569642d3116626c616b65332d636f6c6c656374696f6e2d68617368802003",
        ),
        (
            "msg_project_request",
            Msg::ProjectRequest {
                project_id: "proj-1".to_string(),
                package_id: "hub-pkg-1".to_string(),
            }
            .encode()
            .unwrap(),
            "060670726f6a2d31096875622d706b672d31",
        ),
    ]
}

/// Wire-frozen: assert every serialized wire type's byte image equals its pinned
/// literal. A failure here is a BREAKING wire change — see THE RULE at the top of
/// this file (bump `SYNC_ALPN`, add a new golden set; never silently re-pin).
#[test]
fn wire_format_is_frozen() {
    for (name, actual, expected_hex) in golden_cases() {
        assert_eq!(
            hex(&actual),
            expected_hex,
            "WIRE DRIFT on `{name}`: the postcard byte image changed. This is a \
             BREAKING wire change — bump SYNC_ALPN (sharing/iroh/mod.rs) and add a \
             new golden set; do NOT silently re-pin this literal."
        );
    }
}

/// Guard against forgetting a case: the count of pinned wire types. Bump this
/// deliberately when adding a new wire type + its golden (never to silence it).
#[test]
fn all_wire_types_are_pinned() {
    // 8 embedded/standalone samples + 7 Msg variants = 15.
    assert_eq!(golden_cases().len(), 15, "add a golden case for every new wire type");
}

/// One-off generator: prints `GOLDEN name = "hex"` for every sample so the pinned
/// literals above can be (re)captured from HEAD on a DELIBERATE wire bump. Not a
/// guard — `#[ignore]`d in normal runs.
/// Run with: `cargo test -p athenaeum-core __wire_golden_generate -- --ignored --nocapture`.
#[test]
#[ignore]
fn __wire_golden_generate() {
    for (name, bytes, _) in golden_cases() {
        println!("GOLDEN {name} = \"{}\"", hex(&bytes));
    }
}
