//! The per-frame local state machine (spec §9.4 exactly, plus plan P10's
//! gap-closing edges and the Task 9 rulings). Pure: `(origin, from, event)
//! → to`. Reporting follows from `set_local_state` (servable ↔ not servable
//! → outbox); this module never touches the catalog or the disk.
//!
//! Two edges need a caller-side refinement the table cannot make on its own:
//!
//! - `(Idle, Reincluded)` answers `Wanted`; the caller moves the row to
//!   `Held` instead when the row is known to be seeded and its landed file
//!   stats the same (spec §9.4 "Idle ──re-included──▶ Held or Wanted (stat +
//!   hash decide)"); otherwise the storage engine hashes the file off the
//!   write path — see `db::collab_frames::reinclude`.
//! - `(Wanted, ContentChanged)` answers `Quarantined`: the engine raises it
//!   only for a re-included frame whose verified file (it still carries the
//!   stamp recorded at this version) was edited while it was idle (I9, L5).
//! - `(Quarantined, StampDrift | FileBack)` answers `Held`: the edited bytes
//!   were put back (the hash matches the current version again). The caller
//!   only raises those events after a hash check.

use crate::collab::storage::deletions::DeletionRuling;
use crate::db::collab_frames::{FrameOrigin, LocalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEvent {
    /// A fetch verified and replaced the file (Task 11's landing).
    Landed,
    /// The file at the landed path is gone (settled).
    FileGone,
    /// A file matching the current version (by hash) is back at the landed
    /// path, or was found elsewhere in the root.
    FileBack,
    /// The stamp moved but the bytes hash to the recorded content.
    StampDrift,
    /// The bytes changed in place.
    ContentChanged,
    /// The L4 ruling on a settled deletion.
    Ruled(DeletionRuling),
    /// The user chose "Re-fetch" / "Re-fetch original".
    Refetch,
    /// The user chose "Stop keeping".
    StopKeeping,
    /// The user deleted a changed (quarantined) file.
    DeleteChanged,
    /// The user chose "Keep again" (L6).
    KeepAgain,
    /// The user put a declined frame's file back (hash matches).
    PutBack,
    /// The file moved inside the root (re-adopted by hash).
    Moved,
    /// The manifest made a new content version current.
    NewVersion { same_bytes: bool },
    /// Excluded / lost project / policy drop.
    Excluded,
    /// Re-included (published ∧ accepted ∧ allowed ∧ matching again).
    Reincluded,
}

/// `None` = the event does not apply in that state (the caller leaves the
/// row alone); `Some(from)` = it applies and changes nothing.
pub fn transition(origin: FrameOrigin, from: LocalState, ev: StateEvent) -> Option<LocalState> {
    use LocalState::*;
    use StateEvent::*;
    if origin == FrameOrigin::Own {
        return match (from, ev) {
            (OwnHeld, FileGone) => Some(OwnMissing),
            (OwnMissing, FileBack) => Some(OwnHeld),
            (OwnHeld, ContentChanged) => Some(OwnChanged),
            // the same bytes came back (or were only touched)
            (OwnChanged, StampDrift) | (OwnHeld, StampDrift) => Some(OwnHeld),
            (OwnHeld | OwnMissing | OwnChanged, NewVersion { .. }) => Some(from),
            (OwnHeld, Moved) => Some(OwnHeld),
            _ => None,
        };
    }
    match (from, ev) {
        (Wanted, Landed) => Some(Held),
        (Held, FileGone) => Some(Missing),
        (Missing | Wanted | AwaitingChoice, FileBack) => Some(Held),
        // Task 9 ruling: the edited bytes were put back.
        (Quarantined, StampDrift | FileBack) => Some(Held),
        (Missing | Wanted, Ruled(DeletionRuling::AwaitChoice)) => Some(AwaitingChoice),
        (Missing, Ruled(DeletionRuling::Refetch)) => Some(Wanted),
        (AwaitingChoice, Refetch) => Some(Wanted),
        (AwaitingChoice, StopKeeping) => Some(NotKept),
        (Held, StampDrift) => Some(Held),
        (Held, ContentChanged) => Some(Quarantined),
        // fix round 1: a re-included, verified file edited while idle
        (Wanted, ContentChanged) => Some(Quarantined),
        (Quarantined, Refetch) => Some(Wanted),
        (Quarantined, DeleteChanged) => Some(NotKept),
        (NotKept, KeepAgain) => Some(Wanted),
        (NotKept, PutBack) => Some(Held),
        (Held, Moved) => Some(Held),
        (Held, NewVersion { same_bytes: true }) => Some(Held),
        (Held, NewVersion { same_bytes: false }) => Some(Wanted),
        // L5/L6: a quarantined file waits for the user, a decline survives
        // versions, a choice now applies to the new version.
        (Wanted | Missing | AwaitingChoice | Quarantined | NotKept | Idle, NewVersion { .. }) => {
            Some(from)
        }
        (Wanted | Held | AwaitingChoice | Missing, Excluded) => Some(Idle),
        // Refined to `Held` by the caller when stat + hash confirm the file.
        (Idle, Reincluded) => Some(Wanted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab_frames::{FrameOrigin::*, LocalState::*};
    use StateEvent::*;

    #[test]
    fn replica_edges_of_the_spec_diagram() {
        let cases: &[(LocalState, StateEvent, Option<LocalState>)] = &[
            (Wanted, Landed, Some(Held)),
            (Held, FileGone, Some(Missing)),
            (Missing, FileBack, Some(Held)),
            (Wanted, FileBack, Some(Held)), // the file is back (or found by hash) before the fetch
            (AwaitingChoice, FileBack, Some(Held)),
            (Idle, FileBack, None), // an excluded frame is never served
            (Missing, Ruled(DeletionRuling::Refetch), Some(Wanted)),
            (
                Missing,
                Ruled(DeletionRuling::AwaitChoice),
                Some(AwaitingChoice),
            ),
            (
                Wanted,
                Ruled(DeletionRuling::AwaitChoice),
                Some(AwaitingChoice),
            ), // mass pull-in of a re-fetch in progress
            (AwaitingChoice, Refetch, Some(Wanted)),
            (AwaitingChoice, StopKeeping, Some(NotKept)),
            (
                AwaitingChoice,
                NewVersion { same_bytes: false },
                Some(AwaitingChoice),
            ),
            (Held, StampDrift, Some(Held)),
            (Held, ContentChanged, Some(Quarantined)),
            (Wanted, ContentChanged, Some(Quarantined)), // re-included, edited while idle (fix round 1)
            (Missing, ContentChanged, None),
            (Quarantined, Refetch, Some(Wanted)),
            (Quarantined, DeleteChanged, Some(NotKept)),
            (
                Quarantined,
                NewVersion { same_bytes: false },
                Some(Quarantined),
            ),
            // Task 9 ruling: the edited bytes were put back
            (Quarantined, StampDrift, Some(Held)),
            (Quarantined, FileBack, Some(Held)),
            (NotKept, KeepAgain, Some(Wanted)),
            (NotKept, PutBack, Some(Held)),
            (Held, Moved, Some(Held)),
            (Held, NewVersion { same_bytes: false }, Some(Wanted)),
            (Held, NewVersion { same_bytes: true }, Some(Held)),
            (Wanted, Excluded, Some(Idle)),
            (Held, Excluded, Some(Idle)),
            (AwaitingChoice, Excluded, Some(Idle)),
            (Idle, Reincluded, Some(Wanted)), // refined to Held by stat + hash
            // things that must never happen
            (Quarantined, Landed, None), // nothing lands over a quarantined file (P13)
            (Quarantined, Excluded, None), // an excluded frame's edited file still waits for the user
            (NotKept, NewVersion { same_bytes: false }, Some(NotKept)), // a decline survives versions (L6)
            (NotKept, Excluded, None),
            (Held, KeepAgain, None),
            (Idle, Landed, None),
            (Held, Reincluded, None),
        ];
        for (from, ev, want) in cases {
            assert_eq!(transition(Replica, *from, *ev), *want, "{from:?} + {ev:?}");
        }
    }

    #[test]
    fn own_frames_are_never_refetched_or_quarantined_as_replicas() {
        assert_eq!(transition(Own, OwnHeld, FileGone), Some(OwnMissing));
        assert_eq!(transition(Own, OwnMissing, FileBack), Some(OwnHeld));
        assert_eq!(transition(Own, OwnHeld, ContentChanged), Some(OwnChanged));
        assert_eq!(transition(Own, OwnChanged, StampDrift), Some(OwnHeld)); // the same bytes came back
        assert_eq!(transition(Own, OwnHeld, StampDrift), Some(OwnHeld));
        assert_eq!(
            transition(Own, OwnMissing, Ruled(DeletionRuling::Refetch)),
            None
        );
        assert_eq!(
            transition(Own, OwnHeld, NewVersion { same_bytes: true }),
            Some(OwnHeld)
        );
        assert_eq!(transition(Own, OwnHeld, Excluded), None);
        assert_eq!(transition(Own, OwnHeld, Moved), Some(OwnHeld));
    }
}
