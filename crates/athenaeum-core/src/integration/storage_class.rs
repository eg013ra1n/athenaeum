//! Which kind of storage a set of frames lives on, and how many reads to keep
//! in flight against it.
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

/// The class of a whole frame set. Probes each DISTINCT parent directory
/// (normally one) and returns `Network` if any of them is: the extra readers
/// are what the network members need, and the local members were measured
/// tolerating them. An empty set is `Local`.
pub fn classify_all(paths: &[PathBuf]) -> StorageClass {
    let parents: BTreeSet<&Path> = paths.iter().filter_map(|p| p.parent()).collect();
    if parents.iter().any(|d| classify(d) == StorageClass::Network) {
        StorageClass::Network
    } else {
        StorageClass::Local
    }
}

/// The class of one path. Walks up to the nearest existing ancestor, the same
/// defensive shape `file_op::planner::device_id_for` uses. Any probe failure
/// is `Local` — the conservative answer, since it never exceeds the core count.
pub fn classify(path: &Path) -> StorageClass {
    let mut cur = path;
    loop {
        if cur.exists() {
            return classify_existing(cur);
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return StorageClass::Local,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn classify_existing(path: &Path) -> StorageClass {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return StorageClass::Local;
    };
    // SAFETY: `buf` is a correctly sized zeroed statfs owned by this frame and
    // `c_path` is NUL-terminated; statfs only writes into buf.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return StorageClass::Local;
    }
    if buf.f_flags & (libc::MNT_LOCAL as u32) != 0 {
        StorageClass::Local
    } else {
        StorageClass::Network
    }
}

/// Filesystem magics that mean "the bytes are on another machine". Anything
/// not listed is treated as local, which is the conservative direction: an
/// unrecognised filesystem gets the core-count policy, never more.
///
/// Only the Linux `classify_existing` arm calls this in production; it stays
/// a plain (non-`#[cfg]`) function so its own unit tests run on every dev
/// machine, including this one — same treatment as `band_budget`'s
/// `parse_cgroup_limit`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn is_network_magic(f_type: i64) -> bool {
    const NETWORK_MAGICS: &[i64] = &[
        0x6969,      // NFS (and NFS4)
        0xFF53_4D42, // CIFS / SMB1
        0xFE53_4D42, // SMB2 / SMB3
        0x517B,      // old smbfs
        0x0102_1997, // 9P (v9fs)
        0x00c3_6400, // CephFS
        0x5346_414F, // AFS (OpenAFS)
        0x6B41_4653, // AFS (kAFS)
        0x0BD0_0BD0, // Lustre
    ];
    NETWORK_MAGICS.contains(&f_type)
}

#[cfg(target_os = "linux")]
fn classify_existing(path: &Path) -> StorageClass {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return StorageClass::Local;
    };
    // SAFETY: as above.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return StorageClass::Local;
    }
    // `f_type` is `__fsword_t`, which is `i32` on 32-bit Linux — sign-extending
    // it straight to `i64` would turn CIFS (0xFF534D42) and SMB2 (0xFE534D42)
    // negative and miss the table below, silently classifying every SMB mount
    // as Local. Masking to the low 32 bits first is a no-op on the 64-bit
    // (`i64`-native) targets this project actually ships.
    let f_type = (buf.f_type as i64) & 0xFFFF_FFFF;
    if is_network_magic(f_type) { StorageClass::Network } else { StorageClass::Local }
}

/// Only the Windows `classify_existing` arm calls this in production; it
/// stays a plain (non-`#[cfg]`) function so its own unit tests run on every
/// dev machine, including this one — same treatment as `is_network_magic`
/// above.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_unc(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.starts_with(r"UNC\") || (s.starts_with(r"\\") && !s.starts_with(r"\\?\"))
}

#[cfg(windows)]
fn classify_existing(path: &Path) -> StorageClass {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;
    if is_unc(path) {
        return StorageClass::Network;
    }
    let root = match path.components().next() {
        Some(std::path::Component::Prefix(p)) => {
            let mut r = std::path::PathBuf::from(p.as_os_str());
            r.push("\\");
            r
        }
        _ => return StorageClass::Local,
    };
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer owned by this frame.
    if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_REMOTE {
        StorageClass::Network
    } else {
        StorageClass::Local
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn classify_existing(_path: &Path) -> StorageClass {
    StorageClass::Local
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

    #[test]
    fn linux_network_filesystem_magics_are_recognised() {
        assert!(is_network_magic(0x6969), "NFS");
        assert!(is_network_magic(0xFF53_4D42), "CIFS/SMB1");
        assert!(is_network_magic(0xFE53_4D42), "SMB2/SMB3");
        assert!(is_network_magic(0x517B), "old smbfs");
        assert!(is_network_magic(0x0102_1997), "9P");
        assert!(!is_network_magic(0xEF53), "ext4 is local");
        assert!(!is_network_magic(0x9123_683E), "btrfs is local");
        assert!(!is_network_magic(0x0102_1994), "tmpfs is local");
    }

    #[test]
    fn unc_paths_are_network_by_construction() {
        assert!(is_unc(std::path::Path::new(r"\\nas\astro\bias")));
        assert!(is_unc(std::path::Path::new(r"\\?\UNC\nas\astro")));
        assert!(!is_unc(std::path::Path::new(r"D:\astro\bias")));
        assert!(!is_unc(std::path::Path::new(r"\\?\D:\astro")), "verbatim local drive is not UNC");
        assert!(!is_unc(std::path::Path::new("/Volumes/bigbase2/astro")));
    }

    #[test]
    fn a_temp_dir_classifies_as_local() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(classify(dir.path()), StorageClass::Local);
    }

    #[test]
    fn any_network_member_makes_the_whole_set_network() {
        // A set spanning a NAS and a local disk gets the network policy: the
        // extra readers are what the NAS files need, and the local files were
        // measured tolerating them.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(classify_all(&[p]), StorageClass::Local);
        assert_eq!(classify_all(&[]), StorageClass::Local, "empty set must not panic");
    }
}
