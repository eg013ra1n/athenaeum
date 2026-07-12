//! Pure want/candidate split for the pre-transfer dedup handshake (spec §7).
//! No transport, no DB — the catalog lookups live in the responder (api/db);
//! this is the decision logic, unit-tested in isolation.
use crate::sharing::iroh::proto::{FullHashEntry, OfferEntry};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantSplit {
    pub want: Vec<String>,
    pub candidates: Vec<String>,
}

/// `want` = offered entries whose `sampling_hash` is NOT present locally
/// (definitely absent); `candidates` = `sampling_hash` IS present (possible
/// duplicate, must full-confirm). Input order is preserved within each bucket.
pub fn partition_offer(
    entries: &[OfferEntry],
    local_sampling_present: &HashSet<String>,
) -> WantSplit {
    let mut want = Vec::new();
    let mut candidates = Vec::new();
    for e in entries {
        if local_sampling_present.contains(&e.sampling_hash) {
            candidates.push(e.rel_path.clone());
        } else {
            want.push(e.rel_path.clone());
        }
    }
    WantSplit { want, candidates }
}

/// For candidates: a candidate is a TRUE duplicate iff its full xxh3 is among
/// the full hashes of the receiver's local files that share its sampling hash.
/// Returns the `rel_path`s that are STILL wanted (sampling false-positives).
/// `local_full_by_sampling` maps a `sampling_hash` → the set of full xxh3 of
/// local files carrying that sampling hash.
pub fn confirm_candidates(
    entries: &[FullHashEntry],
    local_full_by_sampling: &HashMap<String, HashSet<String>>,
) -> Vec<String> {
    entries
        .iter()
        .filter(|e| {
            // true duplicate iff a local file with the same sampling hash also
            // has the same FULL hash
            !local_full_by_sampling
                .get(&e.sampling_hash)
                .is_some_and(|set| set.contains(&e.xxh3_full))
        })
        .map(|e| e.rel_path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn oe(rel: &str, s: &str) -> OfferEntry {
        OfferEntry {
            rel_path: rel.into(),
            sampling_hash: s.into(),
            byte_size: 1,
        }
    }
    fn fe(rel: &str, s: &str, f: &str) -> FullHashEntry {
        FullHashEntry {
            rel_path: rel.into(),
            sampling_hash: s.into(),
            xxh3_full: f.into(),
        }
    }

    #[test]
    fn absent_sampling_is_want_present_is_candidate() {
        let offered = [oe("new.fits", "aaaa"), oe("maybe.fits", "bbbb")];
        let local: HashSet<String> = ["bbbb".to_string()].into_iter().collect();
        let split = partition_offer(&offered, &local);
        assert_eq!(split.want, vec!["new.fits".to_string()]);
        assert_eq!(split.candidates, vec!["maybe.fits".to_string()]);
    }

    #[test]
    fn true_dup_dropped_false_positive_wanted() {
        // candidate "dup" full-matches a local full hash → dropped; "collide"
        // samples-match but full differs → still wanted.
        let cands = [
            fe("dup.fits", "bbbb", "1111111111111111"),
            fe("collide.fits", "bbbb", "2222222222222222"),
        ];
        let mut local_full: HashMap<String, HashSet<String>> = HashMap::new();
        local_full.insert(
            "bbbb".into(),
            ["1111111111111111".to_string()].into_iter().collect(),
        );
        let still = confirm_candidates(&cands, &local_full);
        assert_eq!(still, vec!["collide.fits".to_string()]); // dup dropped, collide kept
    }
}
