//! The per-request serve check (spec §9.3, I9; plan P14). A pure decision;
//! the collab provider consumer (`sharing::iroh::spawn_collab_provider_events`)
//! asks it before every get on the collab store.
//!
//! A get is served only when all four conditions hold: the collaboration
//! storage is available, the requested hash is the CURRENT version of a
//! frame this device holds, the file on disk still carries the `size:mtime`
//! recorded when it was held (within the 2 s tolerance), and the upload
//! stream limit has room. A stamp mismatch refuses the get and queues an
//! immediate local check of that frame (the storage engine re-hashes it).

use crate::collab::storage::sweep::Stamp;

/// A frame this device serves from the collab store: the row whose current
/// `blake3` matched the request, where its file lives and the stamp recorded
/// when it became servable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeRecord {
    pub project_id: String,
    pub frame_uuid: String,
    pub path: std::path::PathBuf,
    pub stamp: Stamp,
}

/// The verdict of [`decide`]. Every refusal is answered on the wire before
/// any byte is read or written: `RefuseLimit` as `ERR_LIMIT`, the others as
/// `ERR_PERMISSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeDecision {
    Serve,
    /// The file on disk no longer carries the recorded stamp (edited in
    /// place, replaced, or gone).
    RefuseMismatch,
    /// No servable row has this hash as its current version.
    RefuseNotHeld,
    /// The collaboration storage is not available (marker, read-only,
    /// unmounted).
    RefuseUnavailable,
    /// The upload stream limit is reached.
    RefuseLimit,
}

/// The serve check. `observed` is a fresh stat of `rec.path` (`None` = the
/// file is gone or unreadable). Storage availability is checked first, then
/// the record, then the stamp, then the stream limit — so a request that
/// would be refused anyway never counts against the limit.
pub fn decide(
    rec: Option<&ServeRecord>,
    observed: Option<Stamp>,
    serving: bool,
    streams_in_use: usize,
    limit: usize,
) -> ServeDecision {
    if !serving {
        return ServeDecision::RefuseUnavailable;
    }
    let Some(rec) = rec else {
        return ServeDecision::RefuseNotHeld;
    };
    match observed {
        Some(s) if rec.stamp.matches(&s) => {}
        _ => return ServeDecision::RefuseMismatch,
    }
    if streams_in_use >= limit {
        return ServeDecision::RefuseLimit;
    }
    ServeDecision::Serve
}

/// What the collab provider consumer asks per request. Implementations may
/// block (a catalog read): the consumer calls them on a blocking thread,
/// never on the async runtime.
pub trait ServeOracle: Send + Sync {
    /// The servable row (`held`, or my own frame on disk) whose CURRENT
    /// `blake3` is `blake3_hex`, with a recorded stamp and a landed path.
    /// `None` for anything else — an idle, wanted, quarantined or declined
    /// row, or a superseded version.
    fn lookup(&self, blake3_hex: &str) -> Option<ServeRecord>;
    /// Whether the collaboration storage is available for serving.
    fn serving(&self) -> bool;
    /// The file of `rec` no longer carries its stamp: queue an immediate
    /// local check of that frame. Never blocks.
    fn on_mismatch(&self, rec: &ServeRecord);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> ServeRecord {
        ServeRecord {
            project_id: "p1".into(),
            frame_uuid: "u1".into(),
            path: "/c/x.fits".into(),
            stamp: Stamp {
                size: 10,
                mtime: 100,
            },
        }
    }

    #[test]
    fn the_four_conditions_of_the_serve_check() {
        let ok = Some(Stamp {
            size: 10,
            mtime: 101,
        });
        assert_eq!(decide(Some(&rec()), ok, true, 0, 8), ServeDecision::Serve);
        assert_eq!(
            decide(Some(&rec()), ok, false, 0, 8),
            ServeDecision::RefuseUnavailable
        );
        assert_eq!(decide(None, ok, true, 0, 8), ServeDecision::RefuseNotHeld);
        assert_eq!(
            decide(
                Some(&rec()),
                Some(Stamp {
                    size: 10,
                    mtime: 200
                }),
                true,
                0,
                8
            ),
            ServeDecision::RefuseMismatch
        );
        assert_eq!(
            decide(
                Some(&rec()),
                Some(Stamp {
                    size: 11,
                    mtime: 100
                }),
                true,
                0,
                8
            ),
            ServeDecision::RefuseMismatch
        );
        // file gone
        assert_eq!(
            decide(Some(&rec()), None, true, 0, 8),
            ServeDecision::RefuseMismatch
        );
        assert_eq!(
            decide(Some(&rec()), ok, true, 8, 8),
            ServeDecision::RefuseLimit
        );
    }

    #[test]
    fn a_refused_request_never_reaches_the_limit_check() {
        // At the limit, an unavailable store / unknown hash / changed file
        // still reports its own reason (so a mismatch still queues a check).
        assert_eq!(
            decide(Some(&rec()), None, true, 8, 8),
            ServeDecision::RefuseMismatch
        );
        assert_eq!(decide(None, None, true, 8, 8), ServeDecision::RefuseNotHeld);
        assert_eq!(
            decide(None, None, false, 8, 8),
            ServeDecision::RefuseUnavailable
        );
    }
}
