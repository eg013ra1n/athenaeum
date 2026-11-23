//! The two I/O knobs an integration run needs, resolved together: how much
//! memory a band may use, and how many reads to keep in flight. They are one
//! value because they are decided from the same two inputs — the machine and
//! the storage the frames actually live on — and because passing two loose
//! `usize`s through the engine invites transposing them.

use super::storage_class::StorageClass;

/// The two I/O settings of an integration run: the memory one band may use and the number of reads kept in flight,
/// with the storage class they were derived from.
#[derive(Clone, Copy, Debug)]
pub struct IoPolicy {
    /// The most memory the band buffers may use, in bytes, across all frames of the set.
    pub band_budget_bytes: usize,
    /// The number of frame reads kept in flight at once (a count of reads, not of threads of the rayon pool).
    pub read_concurrency: usize,
    /// Reported in the build's log line so a later measurement on a NAS or an
    /// SSD can be read against the policy that produced it.
    pub storage: StorageClass,
    /// Overlap the next band's read with the current band's combine, and halve
    /// the band to pay for the second resident band, on every storage class
    /// (Task 13c L3). `false` keeps the pre-L3 shape: prefetch and halving only
    /// on `StorageClass::Network`. Only the stacking run (Integrate and the LN
    /// reference build, over registered artifacts that are positional file
    /// reads) sets it; the calibration-library master builds read raw
    /// calibration sets that often sit on USB / HDD and were never measured
    /// with it, so they keep the default.
    pub prefetch_local: bool,
}

impl IoPolicy {
    /// This policy with the every-class prefetch switched on (stacking).
    pub fn with_local_prefetch(mut self) -> Self {
        self.prefetch_local = true;
        self
    }

    /// Whether a band loop under this policy prefetches (and so halves).
    pub fn prefetches(&self) -> bool {
        self.prefetch_local || matches!(self.storage, StorageClass::Network)
    }
}
