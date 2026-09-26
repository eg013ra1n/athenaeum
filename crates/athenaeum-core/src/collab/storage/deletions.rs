//! L4 — replica deletions judged over a rolling 5-minute window, each after
//! a 60 s settle (the settle itself is `watch::SETTLE`). Pure; the storage
//! engine (`api::collab_live::storage_task`) applies the rulings.
//!
//! - ≤ [`MASS_THRESHOLD`] frames in the window → each is re-fetched.
//! - More → ONE non-blocking, reversible choice; every frame deleted in the
//!   window (the earlier ones included) joins it.
//! - A frame deleted a second time within [`SECOND_DELETION`] joins the
//!   choice whatever the count, so the app never fights a user who deletes
//!   one file at a time.

use std::collections::BTreeSet;
use std::time::Duration;

use crate::db::collab_live::DeletionRecord;

pub const WINDOW: Duration = Duration::from_secs(300);
pub const MASS_THRESHOLD: usize = 10;
pub const SECOND_DELETION: Duration = Duration::from_secs(24 * 3600);
pub const LAST_COPY_MIN_OTHERS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionRuling {
    Refetch,
    AwaitChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuledBatch {
    /// One ruling per `(project_id, frame_uuid)` of the batch, in batch order.
    pub rulings: Vec<((String, String), DeletionRuling)>,
    /// More than [`MASS_THRESHOLD`] distinct frames deleted in the window.
    pub mass: bool,
    /// Distinct frames deleted in the window, this batch included.
    pub window_count: usize,
    /// When `mass`: the frames deleted EARLIER in the window (not in this
    /// batch) — they join the choice too.
    pub pull_into_choice: Vec<(String, String)>,
}

pub fn rule_batch(
    history: &[DeletionRecord],
    batch: &[(String, String)],
    now_ms: i64,
) -> RuledBatch {
    let window_start = now_ms - WINDOW.as_millis() as i64;
    let day_start = now_ms - SECOND_DELETION.as_millis() as i64;
    let batch_set: BTreeSet<&(String, String)> = batch.iter().collect();
    let in_window: BTreeSet<(String, String)> = history
        .iter()
        .filter(|r| r.settled_at_ms >= window_start)
        .map(|r| (r.project_id.clone(), r.frame_uuid.clone()))
        .filter(|k| !batch_set.contains(k))
        .collect();
    let window_count = in_window.len() + batch_set.len();
    let mass = window_count > MASS_THRESHOLD;
    let rulings = batch
        .iter()
        .map(|k| {
            let again = history.iter().any(|r| {
                r.project_id == k.0
                    && r.frame_uuid == k.1
                    && r.settled_at_ms >= day_start
                    && r.settled_at_ms < now_ms
            });
            let ruling = if mass || again {
                DeletionRuling::AwaitChoice
            } else {
                DeletionRuling::Refetch
            };
            (k.clone(), ruling)
        })
        .collect();
    RuledBatch {
        rulings,
        mass,
        window_count,
        pull_into_choice: if mass {
            in_window.into_iter().collect()
        } else {
            Vec::new()
        },
    }
}

/// "Stop keeping" warns below this many other holders of the current
/// version, offline holders included (L4, I7).
pub fn last_copy_warning(other_holders_total: usize) -> bool {
    other_holders_total < LAST_COPY_MIN_OTHERS
}

/// No other holder of the current version anywhere: an automatic re-fetch
/// has nowhere to fetch from (L4 "lost frame").
pub fn lost_everywhere(other_holders_total: usize) -> bool {
    other_holders_total == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(u: &str, at: i64) -> DeletionRecord {
        DeletionRecord {
            project_id: "p1".into(),
            frame_uuid: u.into(),
            settled_at_ms: at,
        }
    }
    fn key(u: &str) -> (String, String) {
        ("p1".into(), u.into())
    }
    const MIN: i64 = 60_000;

    #[test]
    fn up_to_ten_in_the_window_are_refetched() {
        let batch: Vec<_> = (0..10).map(|i| key(&format!("u{i}"))).collect();
        let r = rule_batch(&[], &batch, 100 * MIN);
        assert!(!r.mass);
        assert_eq!(r.window_count, 10);
        assert!(r.rulings.iter().all(|(_, d)| *d == DeletionRuling::Refetch));
    }

    #[test]
    fn eleven_in_a_rolling_window_raise_one_choice_and_pull_in_the_earlier_ones() {
        let history: Vec<_> = (0..6)
            .map(|i| rec(&format!("h{i}"), 100 * MIN - 4 * MIN))
            .collect();
        let batch: Vec<_> = (0..5).map(|i| key(&format!("b{i}"))).collect();
        let r = rule_batch(&history, &batch, 100 * MIN);
        assert!(r.mass);
        assert_eq!(r.window_count, 11);
        assert!(r
            .rulings
            .iter()
            .all(|(_, d)| *d == DeletionRuling::AwaitChoice));
        assert_eq!(r.pull_into_choice.len(), 6);
        // history older than 5 minutes does not count
        let old: Vec<_> = (0..6)
            .map(|i| rec(&format!("h{i}"), 100 * MIN - 6 * MIN))
            .collect();
        assert!(!rule_batch(&old, &batch, 100 * MIN).mass);
    }

    #[test]
    fn a_second_deletion_within_a_day_joins_the_choice_whatever_the_count() {
        let history = vec![rec("u1", 100 * MIN - 23 * 60 * MIN)];
        let r = rule_batch(&history, &[key("u1"), key("u2")], 100 * MIN);
        assert!(!r.mass);
        assert_eq!(
            r.rulings,
            vec![
                (key("u1"), DeletionRuling::AwaitChoice),
                (key("u2"), DeletionRuling::Refetch)
            ]
        );
        let old = vec![rec("u1", 100 * MIN - 25 * 60 * MIN)];
        assert_eq!(
            rule_batch(&old, &[key("u1")], 100 * MIN).rulings,
            vec![(key("u1"), DeletionRuling::Refetch)]
        );
    }

    #[test]
    fn a_frame_deleted_twice_in_the_window_counts_once() {
        let history = vec![rec("u1", 100 * MIN - MIN), rec("u1", 100 * MIN - 2 * MIN)];
        let r = rule_batch(&history, &[key("u2")], 100 * MIN);
        assert_eq!(r.window_count, 2);
    }

    #[test]
    fn last_copy_and_lost_everywhere_thresholds() {
        assert!(last_copy_warning(0) && last_copy_warning(1) && !last_copy_warning(2));
        assert!(lost_everywhere(0) && !lost_everywhere(1));
    }
}
