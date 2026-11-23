//! Which kind of storage a set of frames lives on, and how many reads to keep
//! in flight against it.
//!
//! Part of the base layer (no feature needed): `integration::storage_class` re-exports it for the engine's callers.
//!
//! **Two classes, not three.** The obvious third — rotational vs solid state —
//! is deliberately absent. The profiled 7200 rpm SATA drive got *faster* with
//! 10-way concurrency (research §3.1), so there is no measured case for giving
//! a spinning disk fewer readers than an SSD, and answering "is this rotating"
//! on macOS needs IOKit for a verdict nothing would act on. What genuinely
//! inverts the policy is a NETWORK mount: it is latency-bound rather than
//! seek-bound, so it wants MORE outstanding requests than the machine has
//! cores — which is why read concurrency cannot ride the CPU thread pool.
//!
//! Detection is a deterministic OS property on every platform, never a timing
//! probe: a probe is non-deterministic, spends its first bands on a knowingly
//! wrong setting, and would be auto-tuning the smaller of the two levers.

/// The kind of storage a set of frames lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageClass {
    /// Anything the OS calls local: internal or attached, rotating or solid.
    Local,
    /// NFS / SMB / AFP / WebDAV / 9P / a mapped or UNC network drive.
    Network,
}

impl StorageClass {
    /// Short lowercase form for structured logging (the `storage` field on
    /// the build-lifecycle log lines) — so a later measurement on a NAS or an
    /// SSD can be filtered/read against the class the policy actually saw,
    /// without a `{:?}` case mismatch against the rest of the snake_case
    /// field values.
    pub fn as_str(self) -> &'static str {
        match self {
            StorageClass::Local => "local",
            StorageClass::Network => "network",
        }
    }
}

/// Hard ceiling on an explicitly configured reader count. Past this the only
/// thing that grows is the number of requests a server has to fan out.
pub const READ_CONCURRENCY_MAX: usize = 64;

/// How many reads to keep in flight. `configured` is
/// `integration.read_concurrency`; `0` means "decide from the class".
pub fn read_concurrency(class: StorageClass, configured: usize, pool_threads: usize) -> usize {
    if configured != 0 {
        return configured.clamp(1, READ_CONCURRENCY_MAX);
    }
    match class {
        StorageClass::Local => pool_threads.max(1),
        // Latency-bound: the link is filled by outstanding requests, not by
        // cores. Floor 8 so a 4-core box still fills a LAN mount; ceiling 32
        // so a slow uplink is not flooded with streams the server must serve
        // in parallel. Both bounds are reasoned, not measured — the network
        // measurement is an open item, and the setting is the escape hatch
        // until it exists.
        StorageClass::Network => (pool_threads.saturating_mul(2)).clamp(8, 32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_needs_more_readers_than_cores_and_local_does_not() {
        // Local: exactly the CPU pool. The profiled 7200 rpm drive got FASTER
        // with 10-way concurrency (research §3.1), so there is no measured
        // case for giving a spinning disk fewer readers than an SSD.
        assert_eq!(read_concurrency(StorageClass::Local, 0, 10), 10);
        assert_eq!(read_concurrency(StorageClass::Local, 0, 4), 4);

        // Network: latency-bound. A 4-core box must still be able to fill a
        // LAN mount, and a big box must not flood a slow uplink.
        assert_eq!(read_concurrency(StorageClass::Network, 0, 4), 8, "floor");
        assert_eq!(read_concurrency(StorageClass::Network, 0, 10), 20);
        assert_eq!(read_concurrency(StorageClass::Network, 0, 32), 32, "ceiling");
        assert!(
            read_concurrency(StorageClass::Network, 0, 4) > 4,
            "a network mount must be able to exceed the core count — this is exactly what a rayon pool cannot do"
        );

        // An explicit setting wins over both, still bounded.
        assert_eq!(read_concurrency(StorageClass::Network, 6, 10), 6);
        assert_eq!(read_concurrency(StorageClass::Local, 999, 10), READ_CONCURRENCY_MAX);
        assert_eq!(read_concurrency(StorageClass::Local, 0, 0), 1, "never zero readers");
    }
}
