//! Verification + parsing of hub-signed membership snapshots (the
//! cross-account trust anchor, spec §2a).
//!
//! Contract (hub README): verify the signature over the EXACT transported
//! payload bytes against the PINNED hub pubkey, then parse. Clients apply
//! every verified snapshot — content is compared, not the version (device
//! add/revoke changes content without a version bump). Only hub-fetched
//! snapshots ever reach this function.

use anyhow::{bail, Context};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::hub_client::SignedSnapshotWire;

// Serialize too: Task 5 re-serializes the verified member list into the
// cache's members_json (camelCase preserved for the ProjectMemberView parse).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotMember {
    pub account_id: String,
    pub display_name: String,
    pub data_role: String,
    pub coordinator: bool,
    /// Active athenaeum-device pubkeys, base64; ordered by raw pubkey bytes
    /// on the hub (NOT base64-ASCII order) — never assume sortedness here.
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPayload {
    schema: u32,
    #[allow(dead_code)]
    project_id: String,
    membership_version: i64,
    require_approval: bool,
    #[allow(dead_code)]
    issued_at: String,
    members: Vec<SnapshotMember>,
}

#[derive(Debug, Clone)]
pub struct VerifiedSnapshot {
    pub membership_version: i64,
    pub require_approval: bool,
    pub members: Vec<SnapshotMember>,
}

/// Verify the wire snapshot against the pinned hub pubkey and parse it.
/// Every failure is a hard error — a snapshot that does not verify is never
/// partially used.
pub fn verify_and_parse(
    wire: &SignedSnapshotWire,
    pinned_pubkey_b64: &str,
) -> anyhow::Result<VerifiedSnapshot> {
    if wire.pubkey != pinned_pubkey_b64 {
        bail!(
            "snapshot pubkey does not match the pinned hub key (got {}, pinned {})",
            &wire.pubkey,
            pinned_pubkey_b64
        );
    }

    let key_bytes: [u8; 32] = B64
        .decode(pinned_pubkey_b64)
        .context("pinned pubkey is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("pinned pubkey must decode to 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("pinned pubkey is not a valid ed25519 key")?;

    let payload = B64
        .decode(&wire.payload)
        .context("snapshot payload is not valid base64")?;
    let sig_bytes: [u8; 64] = B64
        .decode(&wire.signature)
        .context("snapshot signature is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("snapshot signature must decode to 64 bytes"))?;

    key.verify(&payload, &Signature::from_bytes(&sig_bytes))
        .context("snapshot signature verification failed")?;

    let parsed: SnapshotPayload =
        serde_json::from_slice(&payload).context("verified snapshot payload does not parse")?;
    if parsed.schema != 1 {
        bail!("unsupported snapshot schema {}", parsed.schema);
    }

    Ok(VerifiedSnapshot {
        membership_version: parsed.membership_version,
        require_approval: parsed.require_approval,
        members: parsed.members,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_fixture(key: &SigningKey, payload_json: &serde_json::Value) -> SignedSnapshotWire {
        let bytes = serde_json::to_vec(payload_json).unwrap();
        SignedSnapshotWire {
            payload: B64.encode(&bytes),
            signature: B64.encode(key.sign(&bytes).to_bytes()),
            pubkey: B64.encode(key.verifying_key().to_bytes()),
        }
    }

    fn payload() -> serde_json::Value {
        serde_json::json!({
            "schema": 1, "projectId": "p-1", "membershipVersion": 4, "requireApproval": true,
            "issuedAt": "2026-07-13T00:00:00Z",
            "members": [{"accountId": "a-1", "displayName": "Vilen", "dataRole": "send_receive",
                         "coordinator": true, "nodes": [B64.encode([7u8; 32])]}]
        })
    }

    #[test]
    fn verifies_and_parses_a_good_snapshot() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let wire = signed_fixture(&key, &payload());
        let pinned = wire.pubkey.clone();
        let snap = verify_and_parse(&wire, &pinned).unwrap();
        assert_eq!(snap.membership_version, 4);
        assert!(snap.require_approval);
        assert_eq!(snap.members[0].display_name, "Vilen");
        assert_eq!(snap.members[0].nodes.len(), 1);
    }

    #[test]
    fn rejects_tampered_payload_wrong_key_and_pin_mismatch() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let mut wire = signed_fixture(&key, &payload());
        let pinned = wire.pubkey.clone();

        // Tampered payload bytes → signature check fails.
        let mut tampered = serde_json::to_vec(&payload()).unwrap();
        tampered[10] ^= 0xFF;
        wire.payload = B64.encode(&tampered);
        assert!(verify_and_parse(&wire, &pinned).is_err(), "tampered payload must fail");

        // Signed by a DIFFERENT key but claiming the pinned pubkey → fails.
        let other = SigningKey::from_bytes(&[2u8; 32]);
        let forged = SignedSnapshotWire {
            pubkey: pinned.clone(),
            ..signed_fixture(&other, &payload())
        };
        assert!(verify_and_parse(&forged, &pinned).is_err(), "wrong key must fail");

        // Honest wire but the PIN doesn't match → hard error (never TOFU-drift).
        let honest = signed_fixture(&key, &payload());
        let other_pin = B64.encode(other.verifying_key().to_bytes());
        assert!(verify_and_parse(&honest, &other_pin).is_err(), "pin mismatch must fail");
    }

    #[test]
    fn rejects_unknown_schema() {
        let key = SigningKey::from_bytes(&[1u8; 32]);
        let mut p = payload();
        p["schema"] = serde_json::json!(2);
        let wire = signed_fixture(&key, &p);
        let pinned = wire.pubkey.clone();
        assert!(verify_and_parse(&wire, &pinned).is_err(), "schema 2 must be refused");
    }
}
