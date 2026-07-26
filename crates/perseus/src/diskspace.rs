//! Free-disk-space probe: one [`VolumeInfo`] per unique **volume** behind a set
//! of paths (the capture roots + the data dir).
//!
//! Two capture folders on the same disk are one number to the operator, so the
//! probe de-duplicates by volume identity — `dev()` on unix, the canonicalised
//! volume prefix (`\\?\C:`, `\\?\UNC\server\share`) on Windows — and keeps the
//! first requested path as the entry's label.
//!
//! **A probe failure is never an error.** An observatory routinely runs with an
//! SMB capture root offline; the status page must still render. Every failure
//! path here logs `warn!(root, error, "free-space probe skipped")` and drops the
//! entry, so the caller only ever sees the volumes it could actually measure.
//!
//! **Only the exact requested path is measured** — never an ancestor of it. A
//! path that is absent (an unmounted share, a capture dir the software has not
//! recreated yet) is skipped. Resolving `/Volumes/astro/captures` up to
//! `/Volumes` would report the *boot disk's* free space under the NAS's label,
//! which is worse than showing nothing. The paths this probe is handed
//! (`data_dir`, the configured capture dirs) normally exist, so the skip costs a
//! chip for one poll at most.
//!
//! **Known limitation.** Nothing here reads the mount table, so a mount-point
//! directory that persists while nothing is mounted on it — the ordinary Linux
//! shape, an empty `/mnt/nas` — is indistinguishable from a plain local
//! directory and will measure whatever volume it sits on (usually the root
//! filesystem). That reading belongs to the wrong disk. Telling the two apart
//! needs mount-table inspection (`/proc/self/mountinfo`, `getmntinfo`), which
//! this module deliberately does not do: it is a documented limitation, not
//! something the exact-path rule above can rule out.

use std::path::PathBuf;
use tracing::warn;

/// Capacity of one mounted volume, tagged with the requested path that first
/// landed on it (the label the UI shows, and the key a per-root view matches on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// The requested path this entry was measured for, exactly as passed in —
    /// and, when several requested paths share a volume, the first of them.
    pub root: PathBuf,
    /// Bytes available to this (unprivileged) process — `f_bavail`, not
    /// `f_bfree`: the root-reserved slack is not free space for Perseus.
    pub free_bytes: u64,
    /// Total bytes on the volume.
    pub total_bytes: u64,
}

/// Probe each path's volume, in order, skipping duplicates and failures.
///
/// The returned vec has at most one entry per distinct volume and is ordered by
/// first appearance in `paths`.
pub fn probe_volumes(paths: &[PathBuf]) -> Vec<VolumeInfo> {
    let mut out: Vec<VolumeInfo> = Vec::new();
    let mut seen: Vec<imp::VolumeKey> = Vec::new();

    for requested in paths {
        // Exact path or nothing: an absent path is skipped, never resolved to an
        // ancestor (see the module doc). The `exists` check is only here for the
        // clearer log line — `volume_key` would fail on it anyway.
        if !requested.exists() {
            warn!(
                root = %requested.display(),
                error = "path not present",
                "free-space probe skipped"
            );
            continue;
        }
        let key = match imp::volume_key(requested) {
            Ok(key) => key,
            Err(error) => {
                warn!(root = %requested.display(), %error, "free-space probe skipped");
                continue;
            }
        };
        if seen.contains(&key) {
            continue;
        }

        let (free_bytes, total_bytes) = match imp::capacity(requested) {
            Ok(cap) => cap,
            Err(error) => {
                warn!(root = %requested.display(), %error, "free-space probe skipped");
                continue;
            }
        };

        seen.push(key);
        out.push(VolumeInfo {
            root: requested.clone(),
            free_bytes,
            total_bytes,
        });
    }

    out
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    /// `st_dev` — the same volume identity the file-op move planner uses to tell
    /// a rename from a cross-volume copy.
    pub type VolumeKey = u64;

    pub fn volume_key(path: &Path) -> io::Result<VolumeKey> {
        use std::os::unix::fs::MetadataExt;
        // Follows symlinks on purpose: the volume that matters is the target's.
        Ok(std::fs::metadata(path)?.dev())
    }

    /// `(free_bytes, total_bytes)` via `statvfs`.
    pub fn capacity(path: &Path) -> io::Result<(u64, u64)> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        // SAFETY: `stat` is zero-initialised and only read after a successful
        // call; `cpath` is a valid NUL-terminated C string alive for the call.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // The block counts are expressed in fragments (`f_frsize`); fall back to
        // `f_bsize` on the platforms that leave the fragment size at 0.
        let unit = if stat.f_frsize != 0 {
            stat.f_frsize as u64
        } else {
            stat.f_bsize as u64
        };
        let free = unit.saturating_mul(stat.f_bavail as u64);
        let total = unit.saturating_mul(stat.f_blocks as u64);
        Ok((free, total))
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::io;
    use std::path::{Component, Path};

    /// The canonicalised volume prefix: `\\?\C:` for a drive, `\\?\UNC\server\share`
    /// for a network share (a share is its own volume from the client's side).
    pub type VolumeKey = OsString;

    pub fn volume_key(path: &Path) -> io::Result<VolumeKey> {
        // `canonicalize` returns a verbatim path, so the prefix component is
        // already normalised (case, 8.3 names, mapped-drive → UNC).
        let canonical = std::fs::canonicalize(path)?;
        match canonical.components().next() {
            Some(Component::Prefix(prefix)) => Ok(prefix.as_os_str().to_os_string()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path has no volume prefix",
            )),
        }
    }

    /// `(free_bytes, total_bytes)` via `GetDiskFreeSpaceExW`, which takes *a
    /// directory on the volume* — no drive-letter assumption, so a UNC path
    /// (`\\server\share\...`) is accepted as-is.
    pub fn capacity(path: &Path) -> io::Result<(u64, u64)> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        // A bare UNC share root must carry a trailing backslash
        // (`\\server\share\`) or the call fails; harmless for every other
        // directory, so it is applied uniformly.
        if !matches!(wide.last(), Some(&c) if c == b'\\' as u16 || c == b'/' as u16) {
            wide.push(b'\\' as u16);
        }
        wide.push(0);

        let mut free_to_caller: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive across the call;
        // the three out-params are valid, initialised u64 slots.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_to_caller,
                &mut total,
                &mut total_free,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // `free_to_caller` honours a per-user quota; that is exactly what
        // Perseus may still write.
        Ok((free_to_caller, total))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    pub type VolumeKey = PathBuf;

    pub fn volume_key(path: &Path) -> io::Result<VolumeKey> {
        Ok(path.to_path_buf())
    }

    pub fn capacity(_path: &Path) -> io::Result<(u64, u64)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no free-space probe on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_dedupes_same_volume_and_survives_missing_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let vols = probe_volumes(&[a.clone(), b, PathBuf::from("/definitely/not/mounted/xyz")]);
        assert_eq!(
            vols.len(),
            1,
            "same filesystem → one volume; missing path skipped"
        );
        assert!(vols[0].total_bytes > 0 && vols[0].free_bytes <= vols[0].total_bytes);
    }

    /// The de-duplicated entry keeps the FIRST requested path as its label, not
    /// the path that happened to be stat'd.
    #[test]
    fn entry_is_labelled_with_the_requested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        let vols = probe_volumes(&[a.clone(), tmp.path().to_path_buf()]);
        assert_eq!(vols.len(), 1);
        assert_eq!(vols[0].root, a);
    }

    /// An absent path yields NO entry — the probe never climbs to an ancestor.
    /// Covers both shapes at once: a capture root that IS the mount point
    /// (`/Volumes/astro` gone while unmounted, whose parent is the boot disk)
    /// and a leaf under a live parent. Either fallback would label another
    /// volume's numbers with this path.
    #[test]
    fn absent_path_is_skipped_never_measured_through_an_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let mount_point = tmp.path().join("unmounted_share");
        let leaf_under_live_parent = tmp.path().join("not_yet_created");
        assert!(
            probe_volumes(&[
                mount_point.clone(),
                mount_point.join("captures"),
                leaf_under_live_parent,
            ])
            .is_empty(),
            "an absent path reports nothing, not the volume its parent sits on"
        );
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(probe_volumes(&[]).is_empty());
    }
}
