//! The cursor rules of the event channel (spec I3, §4.4; hub § Wire contract
//! "Cursor rules"). Pure decisions; the applier (`api::collab_live::feed`)
//! performs them.

/// One project's live-feed position: the epoch it was last confirmed under,
/// the `project` cursor (`hub_version`) and the `holders` cursor
/// (`holder_seq`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCursor {
    pub epoch: Option<String>,
    pub version: i64,
    /// −1 = no local holder map (load the snapshot).
    pub holder_seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Apply,
    Ignore,
    CatchUp,
}

/// For a `project` (`head` = version) or `holders` (`head` = seq) event.
pub fn step(cursor: i64, prev: i64, head: i64) -> Step {
    if head <= cursor {
        Step::Ignore
    } else if prev == cursor {
        Step::Apply
    } else {
        Step::CatchUp
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderPlan {
    InSync,
    Delta,
    Snapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloPlan {
    pub epoch_changed: bool,
    pub catch_up_project: bool,
    pub holders: HolderPlan,
}

/// What to do with one project's slice of a `hello` event, given the stored
/// cursor. An epoch mismatch, or the hub's head sitting BELOW the stored
/// cursor on either axis (a restore under the same epoch string), is an
/// epoch change (hub § "Epoch change").
pub fn plan_hello(
    stored: &FeedCursor,
    hub_epoch: &str,
    head_version: i64,
    head_holder_seq: i64,
) -> HelloPlan {
    let epoch_differs = stored.epoch.as_deref().is_some_and(|e| e != hub_epoch);
    let ahead = head_version < stored.version
        || (stored.holder_seq >= 0 && head_holder_seq < stored.holder_seq);
    if epoch_differs || ahead {
        return HelloPlan {
            epoch_changed: true,
            catch_up_project: true,
            holders: HolderPlan::Snapshot,
        };
    }
    let holders = if stored.holder_seq < 0 {
        HolderPlan::Snapshot
    } else if head_holder_seq > stored.holder_seq {
        HolderPlan::Delta
    } else {
        HolderPlan::InSync
    };
    HelloPlan {
        epoch_changed: false,
        catch_up_project: head_version > stored.version,
        holders,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionsPlan {
    InSync,
    CatchUp { project: bool, holders: bool },
    EpochChange,
}

/// What to do with one project's entry of the 60 s `versions` self-heal
/// vector, given the stored cursor. Same "head below cursor = epoch change"
/// rule as [`plan_hello`]; otherwise a head above the stored cursor on
/// either axis is a catch-up of that side.
pub fn plan_versions(stored: &FeedCursor, head_version: i64, head_holder_seq: i64) -> VersionsPlan {
    if head_version < stored.version
        || (stored.holder_seq >= 0 && head_holder_seq < stored.holder_seq)
    {
        return VersionsPlan::EpochChange;
    }
    let project = head_version > stored.version;
    let holders = head_holder_seq > stored.holder_seq;
    if project || holders {
        VersionsPlan::CatchUp { project, holders }
    } else {
        VersionsPlan::InSync
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(epoch: Option<&str>, version: i64, holder_seq: i64) -> FeedCursor {
        FeedCursor {
            epoch: epoch.map(str::to_string),
            version,
            holder_seq,
        }
    }

    #[test]
    fn step_applies_contiguous_ignores_old_and_catches_up_on_a_gap() {
        assert_eq!(step(41, 41, 42), Step::Apply);
        assert_eq!(step(42, 41, 42), Step::Ignore); // duplicate
        assert_eq!(step(45, 41, 42), Step::Ignore); // older than the cursor
        assert_eq!(step(40, 41, 42), Step::CatchUp); // gap
    }

    #[test]
    fn hello_plans() {
        // first connection after the upgrade: no epoch, no holder map
        assert_eq!(
            plan_hello(&cur(None, 40, -1), "e1", 42, 5),
            HelloPlan {
                epoch_changed: false,
                catch_up_project: true,
                holders: HolderPlan::Snapshot
            }
        );
        // resume: same epoch, behind on holders only
        assert_eq!(
            plan_hello(&cur(Some("e1"), 42, 3), "e1", 42, 5),
            HelloPlan {
                epoch_changed: false,
                catch_up_project: false,
                holders: HolderPlan::Delta
            }
        );
        // in sync
        assert_eq!(
            plan_hello(&cur(Some("e1"), 42, 5), "e1", 42, 5),
            HelloPlan {
                epoch_changed: false,
                catch_up_project: false,
                holders: HolderPlan::InSync
            }
        );
        // a new epoch
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e2", 50, 9).epoch_changed);
        // cursor ahead of the hub's head = a restore under the same epoch
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e1", 30, 5).epoch_changed);
        assert!(plan_hello(&cur(Some("e1"), 42, 5), "e1", 42, 2).epoch_changed);
    }

    #[test]
    fn versions_plans() {
        let c = cur(Some("e1"), 42, 5);
        assert_eq!(plan_versions(&c, 42, 5), VersionsPlan::InSync);
        assert_eq!(
            plan_versions(&c, 43, 5),
            VersionsPlan::CatchUp {
                project: true,
                holders: false
            }
        );
        assert_eq!(
            plan_versions(&c, 42, 7),
            VersionsPlan::CatchUp {
                project: false,
                holders: true
            }
        );
        assert_eq!(plan_versions(&c, 41, 5), VersionsPlan::EpochChange);
        // no local holder map yet: a holder head is a catch-up, never an
        // epoch change
        assert_eq!(
            plan_versions(&cur(Some("e1"), 42, -1), 42, 0),
            VersionsPlan::CatchUp {
                project: false,
                holders: true
            }
        );
    }
}
