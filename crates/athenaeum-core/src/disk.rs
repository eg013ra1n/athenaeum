//! Free space on the volume holding a path.
//!
//! Ungated (std/libc/windows-sys only): the stacking plan gate's `space`
//! blocker (render + solver; `stacking::paths` re-exports [`free_bytes`]) and
//! the collab calibrate space check (spec 2026-10-01 §16.1 N40) share this
//! one probe rather than two copies of the platform code.

use std::path::Path;

/// Free bytes available to this process on the volume holding `path`
/// (`statvfs`'s `f_bavail * f_frsize` on unix, the same "available to an
/// unprivileged process" figure `sync::retention::disk_usage_pct` reads;
/// `GetDiskFreeSpaceExW`'s free-bytes-available-to-the-caller on Windows,
/// which honours a per-user quota). `path` must exist — see
/// [`nearest_existing_dir`] for a folder a run is about to create.
///
/// `None` on any error or on another platform — a probe failure must never
/// look like "the disk is full", so the caller treats `None` as "unknown",
/// never zero. Never silent: every failure path `warn!`s before returning
/// `None`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "free space probe failed");
            return None;
        }
    };
    // SAFETY: `stat` is zero-initialised and only read after a successful
    // call; `cpath` is a valid NUL-terminated C string living for the call's
    // duration.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!(path = %path.display(), %error, "free space probe failed");
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// See the unix [`free_bytes`]: the same contract, via
/// `GetDiskFreeSpaceExW`, which takes any directory on the volume.
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        tracing::warn!(path = %path.display(), error = "path contains a NUL", "free space probe failed");
        return None;
    }
    // A UNC share root must end in a backslash (`\\server\share\`) or the
    // call fails; a trailing backslash is harmless on any other directory.
    if !matches!(wide.last(), Some(&c) if c == u16::from(b'\\') || c == u16::from(b'/')) {
        wide.push(u16::from(b'\\'));
    }
    wide.push(0);
    let mut free_to_caller: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer alive across the call;
    // `free_to_caller` is a valid, initialised u64; the two totals the call
    // documents as optional are passed as NULL.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!(path = %path.display(), %error, "free space probe failed");
        return None;
    }
    Some(free_to_caller)
}

/// No probe on this platform: always "unknown".
#[cfg(not(any(unix, windows)))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// The deepest existing directory at or above `path`, never above `root` —
/// the probe target for a folder a run has not created yet (both platform
/// calls need an existing path, and the folder's own volume is its nearest
/// existing ancestor's). The walk stops at `root` (inclusive) so it never
/// measures another volume than the one the tree lives on. `None` when
/// `root` is not an existing directory, `path` is not under `root`, or
/// nothing from `path` up to `root` exists.
pub fn nearest_existing_dir<'a>(path: &'a Path, root: &Path) -> Option<&'a Path> {
    if !root.is_dir() || !path.starts_with(root) {
        return None;
    }
    for p in path.ancestors() {
        if p.is_dir() {
            return Some(p);
        }
        if p == root {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn free_bytes_is_some_for_an_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(free_bytes(tmp.path()).is_some());
    }

    #[test]
    fn nearest_existing_dir_walks_up_to_a_folder_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(nearest_existing_dir(root, root), Some(root));
        let missing = root.join("project").join("me");
        assert_eq!(nearest_existing_dir(&missing, root), Some(root));
        // A file is not a folder: the walk goes past it.
        let file = root.join("f.fits");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(nearest_existing_dir(&file.join("x"), root), Some(root));
    }

    #[test]
    fn nearest_existing_dir_never_goes_above_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        // The root itself is missing: unknown, not the parent volume.
        let root = tmp.path().join("Collab");
        let target = root.join("project").join("me");
        assert_eq!(nearest_existing_dir(&target, &root), None);
        // The root is a file, not a directory.
        let file = tmp.path().join("afile");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(nearest_existing_dir(&file.join("p"), &file), None);
        // A target outside the root is never probed.
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(nearest_existing_dir(&tmp.path().join("other"), &root), None);
    }
}
