//! Reference-frame selection for stacking preparation.
//!
//! The reference frame is the one precise-solved to the catalog; every other
//! member is aligned to it. A good reference has the most detections (highest
//! SNR / least cloud / no tracking hiccup) so the registration quad matching
//! has the richest set of anchors.

/// Select the reference frame from the candidate member list.
///
/// # Algorithm
///
/// 1. If `override_id` is `Some(id)` and that ID appears in `members`,
///    return it immediately.
/// 2. Otherwise pick the member with the highest `detections_count`.
/// 3. Ties are broken by the smallest `frame_id` (stable sort order).
///
/// # Panics
///
/// Panics if `members` is empty — callers must ensure at least one member
/// exists before calling.
pub fn select_reference(
    members: &[(i64, usize)],
    override_id: Option<i64>,
) -> i64 {
    assert!(
        !members.is_empty(),
        "select_reference: members list must not be empty"
    );

    // Override wins if the caller provided one and it is in the list.
    if let Some(oid) = override_id {
        if members.iter().any(|(fid, _)| *fid == oid) {
            return oid;
        }
        tracing::warn!(
            frame_id = oid,
            "override reference frame not in member list, falling back to max-detections selection"
        );
    }

    // Pick the member with the most detections; break ties by smallest frame_id.
    members
        .iter()
        .max_by(|(fid_a, cnt_a), (fid_b, cnt_b)| {
            cnt_a
                .cmp(cnt_b)
                .then(fid_b.cmp(fid_a)) // smaller frame_id wins on tie (reversed)
        })
        .map(|(fid, _)| *fid)
        .unwrap() // safe: asserted non-empty above
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_when_present_in_list() {
        let members = vec![(1, 100), (2, 200), (3, 300)];
        assert_eq!(
            select_reference(&members, Some(1)),
            1,
            "explicit override must be returned even if another frame has more detections"
        );
    }

    #[test]
    fn override_absent_falls_back_to_max_detections() {
        let members = vec![(1, 100), (2, 200), (3, 300)];
        // frame 99 is not in the list → fallback to max detections (frame 3).
        assert_eq!(select_reference(&members, Some(99)), 3);
    }

    #[test]
    fn no_override_picks_max_detections() {
        let members = vec![(1, 50), (2, 80), (3, 60)];
        assert_eq!(select_reference(&members, None), 2);
    }

    #[test]
    fn tie_breaks_by_smallest_frame_id() {
        // Two frames with identical detection counts — the one with the smaller
        // frame_id should be chosen for determinism.
        let members = vec![(10, 200), (5, 200), (20, 200)];
        assert_eq!(
            select_reference(&members, None),
            5,
            "tie in detection count must resolve to smallest frame_id"
        );
    }

    #[test]
    fn single_member_is_always_the_reference() {
        let members = vec![(42, 0)];
        assert_eq!(select_reference(&members, None), 42);
        assert_eq!(select_reference(&members, Some(42)), 42);
    }

    #[test]
    fn override_zero_detections_still_wins() {
        // The override must win even if that frame happens to have zero
        // detections — the caller has a deliberate reason (e.g. it is
        // the user-selected master frame).
        let members = vec![(1, 500), (2, 0)];
        assert_eq!(select_reference(&members, Some(2)), 2);
    }
}
