//! Spec 2026-09-28 §8.1 — the ONE derivation of a contributor's own frame
//! state (per-frame spec §3.3), used by the frame set's Project block and
//! column AND the project page's own-frames table.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContributorState {
    NotPublished,
    FailsGate,
    PendingApproval,
    Published,
    UpdatePending,
    Rejected,
    PublishedNotOnDisk,
    PublishedNowFailsGate,
    Prepared,
    Withheld,
}

impl ContributorState {
    pub fn key(&self) -> &'static str {
        match self {
            Self::NotPublished => "notPublished",
            Self::FailsGate => "failsGate",
            Self::PendingApproval => "pendingApproval",
            Self::Published => "published",
            Self::UpdatePending => "updatePending",
            Self::Rejected => "rejected",
            Self::PublishedNotOnDisk => "publishedNotOnDisk",
            Self::PublishedNowFailsGate => "publishedNowFailsGate",
            Self::Prepared => "prepared",
            Self::Withheld => "withheld",
        }
    }
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::NotPublished => "—",
            Self::FailsGate => "fails gate",
            Self::PendingApproval => "pending",
            Self::Published => "published",
            Self::UpdatePending => "update",
            Self::Rejected => "rejected",
            Self::PublishedNotOnDisk => "not on disk",
            Self::PublishedNowFailsGate => "now fails",
            Self::Prepared => "to review",
            Self::Withheld => "withheld",
        }
    }
}

pub const WITHHELD_REASON: &str = "Withheld by you";
pub const BLACK_HOLE_REASON: &str = "In the Black Hole";

/// Spec 2026-10-01 §4.1: the local facts of a frame with no own row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalFacts {
    pub withheld: bool,
    pub black_holed: bool,
    /// A prepared row exists, its recipe is current and its file is at the
    /// recorded size/mtime (plan W2).
    pub prepared_current: bool,
}

pub struct OwnRowFacts<'a> {
    pub state: &'a str,
    pub content_version: i32,
    pub recipe_hash: Option<&'a str>,
    pub on_disk: bool,
    pub reject_reason: Option<&'a str>,
}

/// `current_recipe` is what a publish would compute now (`None` when the
/// light cannot be resolved); `gate_reason` is the row's first failure.
pub fn derive(
    own: Option<OwnRowFacts<'_>>,
    local: LocalFacts,
    current_recipe: Option<&str>,
    gate_publishable: bool,
    gate_reason: Option<&str>,
) -> (ContributorState, Option<String>) {
    let Some(row) = own else {
        if local.withheld {
            return (
                ContributorState::Withheld,
                Some(WITHHELD_REASON.to_string()),
            );
        }
        if local.black_holed {
            return (
                ContributorState::FailsGate,
                Some(BLACK_HOLE_REASON.to_string()),
            );
        }
        if !gate_publishable {
            return (ContributorState::FailsGate, gate_reason.map(str::to_string));
        }
        if local.prepared_current {
            return (ContributorState::Prepared, None);
        }
        return (ContributorState::NotPublished, None);
    };
    // The hub's own-row `state` is one of "pending"/"published"/"rejected"
    // (the manifest wire contract); anything else is a hub protocol this
    // build does not know yet. Never silently treated as "published" without
    // a trace — the fall-through below still resolves it that way (fail-open,
    // since a contributor's own frame should not go blank on an unrecognized
    // state), but the drift must be visible.
    if !matches!(row.state, "rejected" | "published" | "pending") {
        tracing::warn!(
            state = row.state,
            "own contributor state: unknown row state; treated as published"
        );
    }
    if row.state == "rejected" {
        return (
            ContributorState::Rejected,
            row.reject_reason.map(str::to_string),
        );
    }
    let recipe_moved = matches!((row.recipe_hash, current_recipe), (Some(a), Some(b)) if a != b);
    if row.state == "published" && !row.on_disk {
        return (ContributorState::PublishedNotOnDisk, None);
    }
    if recipe_moved {
        return (ContributorState::UpdatePending, None);
    }
    if row.state == "pending" {
        return (ContributorState::PendingApproval, None);
    }
    if !gate_publishable {
        return (
            ContributorState::PublishedNowFailsGate,
            gate_reason.map(str::to_string),
        );
    }
    (
        ContributorState::Published,
        Some(format!("v{}", row.content_version)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn own<'a>(state: &'a str, recipe: Option<&'a str>, on_disk: bool) -> OwnRowFacts<'a> {
        OwnRowFacts {
            state,
            content_version: 3,
            recipe_hash: recipe,
            on_disk,
            reject_reason: None,
        }
    }
    #[test]
    fn local_facts_follow_the_spec_order() {
        let none = LocalFacts::default();
        // 1. An own row wins over every local fact.
        let all = LocalFacts {
            withheld: true,
            black_holed: true,
            prepared_current: true,
        };
        let (s, _) = derive(
            Some(own("published", Some("r1"), true)),
            all,
            Some("r1"),
            true,
            None,
        );
        assert_eq!(s, ContributorState::Published);
        // 2. Withheld beats the Black Hole, the gate and prepared.
        let (s, r) = derive(None, all, None, false, Some("FWHM 3.4″ > 3.0″"));
        assert_eq!(
            (s, r.as_deref()),
            (ContributorState::Withheld, Some(WITHHELD_REASON))
        );
        // 3. The Black Hole is held back with its own reason.
        let bh = LocalFacts {
            black_holed: true,
            prepared_current: true,
            ..none
        };
        let (s, r) = derive(None, bh, None, true, None);
        assert_eq!(
            (s, r.as_deref()),
            (ContributorState::FailsGate, Some(BLACK_HOLE_REASON))
        );
        // 4. A failing gate beats a prepared row.
        let prep = LocalFacts {
            prepared_current: true,
            ..none
        };
        let (s, r) = derive(None, prep, None, false, Some("no analysis"));
        assert_eq!(
            (s, r.as_deref()),
            (ContributorState::FailsGate, Some("no analysis"))
        );
        // 5. Prepared and current -> Prepared.
        let (s, _) = derive(None, prep, None, true, None);
        assert_eq!(s, ContributorState::Prepared);
        // 6. Otherwise Ready.
        let (s, _) = derive(None, none, None, true, None);
        assert_eq!(s, ContributorState::NotPublished);
        assert_eq!(ContributorState::Prepared.key(), "prepared");
        assert_eq!(ContributorState::Withheld.short_label(), "withheld");
    }
    #[test]
    fn every_row_of_the_table() {
        assert_eq!(
            derive(None, LocalFacts::default(), Some("r1"), true, None).0,
            ContributorState::NotPublished
        );
        let (s, r) = derive(
            None,
            LocalFacts::default(),
            Some("r1"),
            false,
            Some("no analysis"),
        );
        assert_eq!(s, ContributorState::FailsGate);
        assert_eq!(r.as_deref(), Some("no analysis"));
        assert_eq!(
            derive(
                Some(own("pending", Some("r1"), true)),
                LocalFacts::default(),
                Some("r1"),
                true,
                None
            )
            .0,
            ContributorState::PendingApproval
        );
        let (s, r) = derive(
            Some(own("published", Some("r1"), true)),
            LocalFacts::default(),
            Some("r1"),
            true,
            None,
        );
        assert_eq!(s, ContributorState::Published);
        assert_eq!(r.as_deref(), Some("v3"));
        assert_eq!(
            derive(
                Some(own("published", Some("r1"), true)),
                LocalFacts::default(),
                Some("r2"),
                true,
                None
            )
            .0,
            ContributorState::UpdatePending
        );
        assert_eq!(
            derive(
                Some(own("pending", Some("r1"), true)),
                LocalFacts::default(),
                Some("r2"),
                true,
                None
            )
            .0,
            ContributorState::UpdatePending
        );
        let (s, r) = derive(
            Some(OwnRowFacts {
                reject_reason: Some("trailed"),
                ..own("rejected", Some("r1"), true)
            }),
            LocalFacts::default(),
            Some("r1"),
            true,
            None,
        );
        assert_eq!(s, ContributorState::Rejected);
        assert_eq!(r.as_deref(), Some("trailed"));
        assert_eq!(
            derive(
                Some(own("published", Some("r1"), false)),
                LocalFacts::default(),
                Some("r1"),
                true,
                None
            )
            .0,
            ContributorState::PublishedNotOnDisk
        );
        let (s, r) = derive(
            Some(own("published", Some("r1"), true)),
            LocalFacts::default(),
            Some("r1"),
            false,
            Some("FWHM 3.4″ > 3.0″"),
        );
        assert_eq!(s, ContributorState::PublishedNowFailsGate);
        assert_eq!(r.as_deref(), Some("FWHM 3.4″ > 3.0″"));
        // Not on disk wins over now-fails; update pending wins over now-fails.
        assert_eq!(
            derive(
                Some(own("published", Some("r1"), false)),
                LocalFacts::default(),
                Some("r1"),
                false,
                Some("x")
            )
            .0,
            ContributorState::PublishedNotOnDisk
        );
        assert_eq!(
            derive(
                Some(own("published", Some("r1"), true)),
                LocalFacts::default(),
                Some("r2"),
                false,
                Some("x")
            )
            .0,
            ContributorState::UpdatePending
        );
        // A recipe the app cannot compute (cannot resolve the light) reads as update pending only if the row had one.
        assert_eq!(
            derive(
                Some(own("published", None, true)),
                LocalFacts::default(),
                Some("r1"),
                true,
                None
            )
            .0,
            ContributorState::Published
        );
        assert_eq!(ContributorState::UpdatePending.key(), "updatePending");
        assert_eq!(
            ContributorState::PublishedNotOnDisk.short_label(),
            "not on disk"
        );
    }
}
