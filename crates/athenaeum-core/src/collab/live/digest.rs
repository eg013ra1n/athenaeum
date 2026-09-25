//! The order-independent claim digest (spec §6.3; hub plan § Wire contract
//! "The claim digest"). Both sides compute it from the device's report
//! stream only, so a version bump never changes it.

use uuid::Uuid;

pub const ZERO_HEX: &str = "00000000000000000000000000000000";

/// First 16 bytes of `blake3(uuid_bytes ‖ content_version as u32 BE)`.
pub fn claim_key(frame_uuid: &Uuid, content_version: u32) -> [u8; 16] {
    let mut input = [0u8; 20];
    input[..16].copy_from_slice(frame_uuid.as_bytes());
    input[16..].copy_from_slice(&content_version.to_be_bytes());
    let h = blake3::hash(&input);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h.as_bytes()[..16]);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaimDigest {
    pub count: i64,
    pub xor: [u8; 16],
}

impl ClaimDigest {
    fn fold(&mut self, u: &Uuid, cv: u32) {
        for (a, b) in self.xor.iter_mut().zip(claim_key(u, cv)) {
            *a ^= b;
        }
    }

    pub fn add(&mut self, u: &Uuid, cv: u32) {
        self.fold(u, cv);
        self.count += 1;
    }

    pub fn remove(&mut self, u: &Uuid, cv: u32) {
        self.fold(u, cv);
        self.count -= 1;
    }

    pub fn hex(&self) -> String {
        self.xor.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The digest of a whole claim set (`(frame_uuid, content_version)`).
    pub fn of_claims<'a>(
        claims: impl IntoIterator<Item = (&'a str, i32)>,
    ) -> anyhow::Result<ClaimDigest> {
        let mut d = ClaimDigest::default();
        for (uuid, cv) in claims {
            let cv = u32::try_from(cv)
                .map_err(|_| anyhow::anyhow!("claim {uuid}: content version {cv} < 0"))?;
            d.add(&uuid_for_digest(uuid), cv);
        }
        Ok(d)
    }
}

/// The UUID a frame id contributes to the digest. Hub frame ids always parse;
/// a non-UUID id (only test fixtures such as `"u1"`) maps to a stable v5 UUID
/// so the app and the fake hub agree.
pub fn uuid_for_digest(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn u(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }
    const A: &str = "00000000-0000-4000-8000-000000000001";
    const B: &str = "00000000-0000-4000-8000-000000000002";
    const C: &str = "00000000-0000-4000-8000-000000000003";

    fn hex16(b: &[u8; 16]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn input_bytes_and_blake3_match_the_hub_vector() {
        let mut input = Vec::new();
        input.extend_from_slice(u(A).as_bytes());
        input.extend_from_slice(&1u32.to_be_bytes());
        let hex: String = input.iter().map(|x| format!("{x:02x}")).collect();
        assert_eq!(hex, "0000000000004000800000000000000100000001");
        assert_eq!(
            blake3::hash(&input).to_hex().as_str(),
            "129cef69895583884285d1404f99e181afc3b63d273a5314902d2ec7bf24183f"
        );
    }

    #[test]
    fn keys_match_the_hub_vectors() {
        assert_eq!(
            hex16(&claim_key(&u(A), 1)),
            "129cef69895583884285d1404f99e181"
        );
        assert_eq!(
            hex16(&claim_key(&u(B), 1)),
            "c3ef44a7f50605a0dee0abca26ecf959"
        );
        assert_eq!(
            hex16(&claim_key(&u(C), 2)),
            "40fc3b9d29d73f9101f6293d9c3b5cc4"
        );
        assert_eq!(
            hex16(&claim_key(&u(C), 1)),
            "b8c7cf6630052129879bab6949f47d5b"
        );
    }

    #[test]
    fn set_digests_match_the_hub_vectors() {
        let d = ClaimDigest::of_claims([(A, 1), (B, 1)]).unwrap();
        assert_eq!(
            (d.count, d.hex().as_str()),
            (2, "d173abce7c5386289c657a8a697518d8")
        );
        let d = ClaimDigest::of_claims([(A, 1), (B, 1), (C, 2)]).unwrap();
        assert_eq!(
            (d.count, d.hex().as_str()),
            (3, "918f90535584b9b99d9353b7f54e441c")
        );
        let d = ClaimDigest::of_claims([(A, 1), (B, 1), (C, 1)]).unwrap();
        assert_eq!(
            (d.count, d.hex().as_str()),
            (3, "69b464a84c56a7011bfed1e320816583")
        );
        assert_eq!(ClaimDigest::default().hex(), ZERO_HEX);
    }

    #[test]
    fn remove_undoes_add_and_order_does_not_matter() {
        let mut d = ClaimDigest::default();
        d.add(&u(C), 2);
        d.add(&u(A), 1);
        d.add(&u(B), 1);
        d.remove(&u(C), 2);
        assert_eq!(d, ClaimDigest::of_claims([(B, 1), (A, 1)]).unwrap());
    }
}
