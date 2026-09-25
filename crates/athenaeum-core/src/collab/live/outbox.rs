//! Building holder reports from the outbox and the claim set (spec §6.2,
//! §6.3; hub § "Holders — PUT holders/self"), and the flush timing. Pure: the
//! DB reads/writes and the hub call live in `api::collab_live::holdings`.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::collab::live::digest::ClaimDigest;
use crate::collab::live::wire::{ClaimWire, HoldersReportWire};
use crate::db::collab_live::{ClaimOp, OutboxRow};

/// The outbox flushes this long after its first unsent entry, unless the hub
/// set another delay through `nextFlushMs` (spec §6.2).
pub const DEFAULT_FLUSH: Duration = Duration::from_millis(1000);
/// The outbox flushes at once when this many entries are pending (spec §6.2).
pub const FLUSH_AT_ENTRIES: usize = 500;
/// The off-the-critical-path digest comparison runs this often (spec §6.3).
pub const DIGEST_CHECK_EVERY: Duration = Duration::from_secs(3600);

/// The outbox coalesced per frame (last op wins): `add` and `remove` are
/// disjoint and sorted by uuid; `max_seq` is the highest journal sequence the
/// rows carried.
#[derive(Debug, Clone, PartialEq)]
pub struct Coalesced {
    pub add: Vec<ClaimWire>,
    pub remove: Vec<String>,
    pub max_seq: i64,
}

/// Coalesce outbox rows per frame: the LAST op of each frame wins (hub rule:
/// "Coalesce your outbox per frame before sending").
pub fn coalesce(rows: &[OutboxRow]) -> Coalesced {
    let mut last: BTreeMap<&str, ClaimOp> = BTreeMap::new();
    let mut max_seq = 0;
    for r in rows {
        last.insert(r.frame_uuid.as_str(), r.op);
        max_seq = max_seq.max(r.seq);
    }
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for (uuid, op) in last {
        match op {
            ClaimOp::Add { content_version } => add.push(ClaimWire {
                uuid: uuid.to_string(),
                content_version,
            }),
            ClaimOp::Remove => remove.push(uuid.to_string()),
        }
    }
    Coalesced {
        add,
        remove,
        max_seq,
    }
}

fn digest_of(claims: &[(String, i32)]) -> anyhow::Result<ClaimDigest> {
    ClaimDigest::of_claims(claims.iter().map(|(u, v)| (u.as_str(), *v)))
}

/// A delta report of the pending outbox `rows`, as of the claim set `claims`
/// read in the SAME read transaction. `reportSeq` is the highest sequence the
/// rows carry; `digest`/`count` describe the whole claim set after it.
///
/// Every frame the outbox touched is reported in its state IN `claims` —
/// normally exactly the coalesced last op, since [`record_claim_change`]
/// writes both. They differ only when a claim-set write that never enters the
/// outbox came after the frame's last outbox row: an implicit claim
/// (announce/version — the claim set holds the hub's newer version) or a
/// refusal drop. Reporting the claim set there keeps the report consistent
/// with the digest it carries, so a stale outbox `add` can never overwrite
/// the hub's implicit claim with an older version.
///
/// [`record_claim_change`]: crate::db::collab_live::record_claim_change
pub fn delta_report(
    rows: &[OutboxRow],
    claims: &[(String, i32)],
) -> anyhow::Result<HoldersReportWire> {
    let c = coalesce(rows);
    let held: HashMap<&str, i32> = claims.iter().map(|(u, v)| (u.as_str(), *v)).collect();
    let mut touched: Vec<&str> = c
        .add
        .iter()
        .map(|a| a.uuid.as_str())
        .chain(c.remove.iter().map(String::as_str))
        .collect();
    touched.sort_unstable();
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for uuid in touched {
        match held.get(uuid) {
            Some(cv) => add.push(ClaimWire {
                uuid: uuid.to_string(),
                content_version: *cv,
            }),
            None => remove.push(uuid.to_string()),
        }
    }
    let d = digest_of(claims)?;
    Ok(HoldersReportWire {
        report_seq: c.max_seq.max(1),
        full: false,
        add,
        remove,
        digest: d.hex(),
        count: d.count,
    })
}

/// A `full: true` report: `add` is the whole claim set as of `report_seq`
/// (which the caller takes fresh, so the hub applies it — a reused sequence
/// is ignored for every frame already stamped with it).
pub fn full_report(claims: &[(String, i32)], report_seq: i64) -> anyhow::Result<HoldersReportWire> {
    let d = digest_of(claims)?;
    Ok(HoldersReportWire {
        report_seq: report_seq.max(1),
        full: true,
        add: claims
            .iter()
            .map(|(u, v)| ClaimWire {
                uuid: u.clone(),
                content_version: *v,
            })
            .collect(),
        remove: Vec::new(),
        digest: d.hex(),
        count: d.count,
    })
}

/// An empty report — a pure digest check (no lock, no write on the hub).
pub fn digest_check(
    claims: &[(String, i32)],
    report_seq: i64,
) -> anyhow::Result<HoldersReportWire> {
    let d = digest_of(claims)?;
    Ok(HoldersReportWire {
        report_seq: report_seq.max(1),
        full: false,
        add: vec![],
        remove: vec![],
        digest: d.hex(),
        count: d.count,
    })
}

/// When one project's outbox is due: `next_flush` after its first unsent
/// entry, or at once at [`FLUSH_AT_ENTRIES`].
#[derive(Debug, Clone)]
pub struct FlushClock {
    next_flush: Duration,
    first_pending: Option<Instant>,
}

impl Default for FlushClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FlushClock {
    pub fn new() -> Self {
        Self {
            next_flush: DEFAULT_FLUSH,
            first_pending: None,
        }
    }

    /// An entry was appended; the first one since the last flush starts the
    /// wait.
    pub fn on_append(&mut self, now: Instant) {
        self.first_pending.get_or_insert(now);
    }

    pub fn due(&self, now: Instant, pending: usize) -> bool {
        pending >= FLUSH_AT_ENTRIES
            || self
                .first_pending
                .is_some_and(|t| now >= t + self.next_flush)
    }

    /// A flush went through; the hub's `nextFlushMs` sets the next wait.
    pub fn flushed(&mut self, next_flush_ms: u64) {
        self.next_flush = Duration::from_millis(next_flush_ms.max(1));
        self.first_pending = None;
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.first_pending.map(|t| t + self.next_flush)
    }

    /// Nothing is pending after all (the outbox turned out empty): stop
    /// waiting, keep the hub's delay.
    pub fn clear(&mut self) {
        self.first_pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab_live::{ClaimOp, OutboxRow};

    fn row(seq: i64, u: &str, op: ClaimOp) -> OutboxRow {
        OutboxRow {
            seq,
            frame_uuid: u.into(),
            op,
        }
    }

    #[test]
    fn last_op_per_frame_wins_and_lists_are_disjoint() {
        let rows = vec![
            row(1, "u1", ClaimOp::Add { content_version: 1 }),
            row(2, "u2", ClaimOp::Add { content_version: 1 }),
            row(3, "u1", ClaimOp::Remove),
            row(4, "u2", ClaimOp::Add { content_version: 2 }),
        ];
        let c = coalesce(&rows);
        assert_eq!(c.max_seq, 4);
        assert_eq!(c.remove, vec!["u1".to_string()]);
        assert_eq!(
            c.add,
            vec![ClaimWire {
                uuid: "u2".into(),
                content_version: 2
            }]
        );
    }

    #[test]
    fn reports_carry_the_digest_of_the_whole_claim_set_after_the_report() {
        let claims = vec![
            ("00000000-0000-4000-8000-000000000001".to_string(), 1),
            ("00000000-0000-4000-8000-000000000002".to_string(), 1),
        ];
        let r = delta_report(
            &[row(
                7,
                "00000000-0000-4000-8000-000000000002",
                ClaimOp::Add { content_version: 1 },
            )],
            &claims,
        )
        .unwrap();
        assert_eq!((r.report_seq, r.full, r.count), (7, false, 2));
        assert_eq!(r.digest, "d173abce7c5386289c657a8a697518d8");
        let f = full_report(&claims, 9).unwrap();
        assert!(f.full && f.remove.is_empty() && f.add.len() == 2 && f.report_seq == 9);
        let e = digest_check(&claims, 0).unwrap();
        assert!(e.add.is_empty() && e.remove.is_empty() && !e.full && e.report_seq == 1);
    }

    #[test]
    fn a_report_states_each_touched_frame_as_the_claim_set_holds_it() {
        // u1: an outbox add of v1, then an implicit version claim of v2 —
        // the report must say v2, never overwrite the hub's claim with v1.
        // u2: an outbox add, then a refusal drop — reported removed.
        // u3: an outbox remove, then an implicit announce claim — reported
        // held.
        let rows = vec![
            row(1, "u1", ClaimOp::Add { content_version: 1 }),
            row(2, "u2", ClaimOp::Add { content_version: 1 }),
            row(3, "u3", ClaimOp::Remove),
        ];
        let claims = vec![("u1".to_string(), 2), ("u3".to_string(), 1)];
        let r = delta_report(&rows, &claims).unwrap();
        assert_eq!(
            r.add,
            vec![
                ClaimWire {
                    uuid: "u1".into(),
                    content_version: 2
                },
                ClaimWire {
                    uuid: "u3".into(),
                    content_version: 1
                },
            ]
        );
        assert_eq!(r.remove, vec!["u2".to_string()]);
        assert_eq!(r.count, 2);
    }

    #[test]
    fn flush_clock_waits_next_flush_or_five_hundred_entries() {
        let t0 = Instant::now();
        let mut c = FlushClock::new();
        assert!(!c.due(t0, 0));
        assert_eq!(c.deadline(), None);
        c.on_append(t0);
        assert_eq!(c.deadline(), Some(t0 + DEFAULT_FLUSH));
        assert!(!c.due(t0 + Duration::from_millis(500), 3));
        assert!(c.due(t0 + Duration::from_millis(1000), 3));
        assert!(c.due(t0, FLUSH_AT_ENTRIES));
        c.flushed(2000);
        c.on_append(t0);
        assert!(!c.due(t0 + Duration::from_millis(1500), 1));
        assert!(c.due(t0 + Duration::from_millis(2000), 1));
    }
}
