//! The authority of change detection (spec §9.2): `size:mtime` with a 2 s
//! tolerance for FAT and SMB, hourly (±25 %) while the watcher is healthy,
//! every 5 minutes while it is not or the store is on a network volume.

use std::path::Path;
use std::time::Duration;

use crate::geometry::ransac::SplitMix64;

pub const MTIME_TOLERANCE_SECS: i64 = 2;
pub const SWEEP_HEALTHY: Duration = Duration::from_secs(3600);
pub const SWEEP_DEGRADED: Duration = Duration::from_secs(300);
pub const SWEEP_JITTER: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub mtime: i64,
}

impl Stamp {
    pub fn of(meta: &std::fs::Metadata) -> Stamp {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Stamp {
            size: meta.len(),
            mtime,
        }
    }

    /// The wave-2 `size_mtime_seen` format, `"<len>:<mtime_secs>"`.
    pub fn parse(s: &str) -> Option<Stamp> {
        let (a, b) = s.split_once(':')?;
        Some(Stamp {
            size: a.parse().ok()?,
            mtime: b.parse().ok()?,
        })
    }

    pub fn encode(&self) -> String {
        format!("{}:{}", self.size, self.mtime)
    }

    pub fn matches(&self, other: &Stamp) -> bool {
        // `abs_diff`, not `(a - b).abs()`: the plain subtraction can overflow
        // `i64` (and panic in a debug build) for the pathological mtimes a
        // corrupt filesystem or a bad clock can produce.
        self.size == other.size && self.mtime.abs_diff(other.mtime) <= MTIME_TOLERANCE_SECS as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatVerdict {
    Same,
    Drifted(Stamp),
    Missing,
    Unreadable(String),
}

pub fn stat_verdict(path: &Path, recorded: Option<Stamp>) -> StatVerdict {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let now = Stamp::of(&meta);
            match recorded {
                Some(r) if r.matches(&now) => StatVerdict::Same,
                _ => StatVerdict::Drifted(now),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StatVerdict::Missing,
        Err(e) => StatVerdict::Unreadable(e.to_string()),
    }
}

pub fn next_sweep_delay(degraded: bool, rng: &mut SplitMix64) -> Duration {
    if degraded {
        return SWEEP_DEGRADED;
    }
    let factor = 1.0 - SWEEP_JITTER + 2.0 * SWEEP_JITTER * rng.next_f64();
    SWEEP_HEALTHY.mul_f64(factor)
}

/// Whether `path` sits on a network mount — the input to the sweep cadence
/// above (spec §9.2): every 5 minutes on a network volume or when this can't
/// be told, hourly otherwise. Fix round 1 (Important ruling): this used to
/// be a second, weaker copy of the platform detection — Windows mapped
/// drive letters read as local, verbatim local paths as network; the Linux
/// `f_type` sign-extended on 32-bit targets; macOS matched on a name list
/// instead of `MNT_LOCAL`; and 9P/CephFS/AFS/Lustre were missing. It now
/// delegates the actual probe to the shared, tested `storage_class::classify`.
///
/// One difference from that function's own contract on purpose: `classify`
/// treats a probe failure as `Local` (the safe direction for read
/// concurrency, which must never exceed the core count on a misread). Here
/// the safe direction is the opposite — falling back to the frequent sweep
/// is cheap, so an unprobeable path (most commonly: the collaboration root
/// itself is momentarily gone, e.g. an unmounted network share) counts as
/// degraded rather than as local.
pub fn is_network_volume(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    crate::storage_class::classify(path) == crate::storage_class::StorageClass::Network
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_round_trip_and_tolerate_two_seconds_of_mtime() {
        let s = Stamp::parse("1048576:1727000000").unwrap();
        assert_eq!(s.encode(), "1048576:1727000000");
        assert!(s.matches(&Stamp {
            size: 1048576,
            mtime: 1727000002
        }));
        assert!(!s.matches(&Stamp {
            size: 1048576,
            mtime: 1727000003
        }));
        assert!(!s.matches(&Stamp {
            size: 1048575,
            mtime: 1727000000
        }));
        assert_eq!(Stamp::parse("junk"), None);
    }

    #[test]
    fn stat_verdicts() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x.fits");
        std::fs::write(&f, b"0123456789").unwrap();
        let st = Stamp::of(&std::fs::metadata(&f).unwrap());
        assert_eq!(stat_verdict(&f, Some(st)), StatVerdict::Same);
        std::fs::write(&f, b"0123456789-longer").unwrap();
        assert!(matches!(
            stat_verdict(&f, Some(st)),
            StatVerdict::Drifted(_)
        ));
        std::fs::remove_file(&f).unwrap();
        assert_eq!(stat_verdict(&f, Some(st)), StatVerdict::Missing);
    }

    #[test]
    fn sweep_cadence_is_hourly_with_jitter_or_five_minutes_when_degraded() {
        let mut rng = crate::geometry::ransac::SplitMix64(9);
        for _ in 0..200 {
            let d = next_sweep_delay(false, &mut rng);
            assert!(
                d >= SWEEP_HEALTHY.mul_f64(1.0 - SWEEP_JITTER)
                    && d <= SWEEP_HEALTHY.mul_f64(1.0 + SWEEP_JITTER),
                "{d:?}"
            );
        }
        assert_eq!(next_sweep_delay(true, &mut rng), SWEEP_DEGRADED);
    }

    #[test]
    fn a_local_temp_dir_is_not_a_network_volume() {
        assert!(!is_network_volume(&std::env::temp_dir()));
        #[cfg(windows)]
        assert!(is_network_volume(Path::new(r"\\nas\share\collab")));
    }

    #[test]
    fn a_path_that_cannot_be_probed_is_degraded() {
        // Fix round 1: an unprobeable path (the common real case is the
        // collaboration root itself momentarily unmounted) must fall back to
        // the frequent sweep, the opposite of `storage_class::classify`'s
        // own "probe failure is Local" contract.
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("does-not-exist");
        assert!(is_network_volume(&gone));
    }

    #[test]
    fn is_network_volume_agrees_with_storage_class() {
        // The whole point of the fold-in: no second, weaker detector.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            is_network_volume(dir.path()),
            crate::storage_class::classify(dir.path())
                == crate::storage_class::StorageClass::Network
        );
    }
}
