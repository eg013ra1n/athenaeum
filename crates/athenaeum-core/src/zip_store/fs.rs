//! File-system helpers shared by `zip_store` and `archive`.

use std::fs::File;
use std::io;
use std::path::Path;

/// True when `e` says the file system does not implement fsync (some SMB/NFS/FUSE mounts answer
/// `ENOTSUP`/`EOPNOTSUPP`, others `EINVAL`).
fn fsync_unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        if let Some(code) = e.raw_os_error() {
            return code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::EINVAL || code == libc::ENOSYS;
        }
    }
    false
}

/// Classifies the result of an fsync: "the file system cannot sync" is not a failure of the write, so it
/// is logged at debug and turned into `Ok`; every other error (EIO, ENOSPC, ...) is returned.
fn classify_sync(result: io::Result<()>, path: &Path) -> io::Result<()> {
    match result {
        Err(e) if fsync_unsupported(&e) => {
            tracing::debug!(path = %path.display(), error = %e, "fsync not supported here; continuing");
            Ok(())
        }
        other => other,
    }
}

/// `file.sync_all()` that tolerates file systems without fsync (network shares). `path` is for the log only.
///
/// On macOS `sync_all` issues `F_FULLFSYNC`, which an SMB mount refuses with `ENOTSUP` while a plain
/// `fsync(2)` still flushes to the server, so the plain call is tried before the sync is given up.
pub(crate) fn sync_best_effort(file: &File, path: &Path) -> io::Result<()> {
    let result = file.sync_all();
    #[cfg(target_os = "macos")]
    let result = match result {
        Err(e) if fsync_unsupported(&e) => {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor belongs to `file`, which outlives the call.
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        other => other,
    };
    classify_sync(result, path)
}

/// Lets a rename replace `target` when Windows marks it read-only. There a rename refuses to replace a file
/// carrying the read-only attribute ("Access is denied"), while on unix a rename never looks at the target's
/// mode, so this is a no-op off Windows. A target that does not exist is fine. Callers replace on the user's
/// request (Overwrite, a rollback onto the original path), so the attribute is cleared rather than reported.
#[cfg(windows)]
pub(crate) fn clear_readonly_for_replace(target: &Path) -> io::Result<()> {
    let mut perms = match std::fs::metadata(target) {
        Ok(meta) => meta.permissions(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !perms.readonly() {
        return Ok(());
    }
    // On Windows this clears FILE_ATTRIBUTE_READONLY only; the unix world-writable caveat does not apply.
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(target, perms)?;
    tracing::info!(path = %target.display(), "cleared the read-only attribute to replace the file");
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn clear_readonly_for_replace(_target: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(e: io::Error) -> io::Result<()> {
        classify_sync(Err(e), Path::new("/x"))
    }

    #[test]
    fn unsupported_kind_is_ok() {
        assert!(classify(io::Error::from(io::ErrorKind::Unsupported)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_errnos_are_ok() {
        for code in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS] {
            assert!(classify(io::Error::from_raw_os_error(code)).is_ok(), "errno {code}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_failures_are_returned() {
        for code in [libc::EIO, libc::ENOSPC] {
            let e = classify(io::Error::from_raw_os_error(code)).unwrap_err();
            assert_eq!(e.raw_os_error(), Some(code));
        }
        assert!(classify(io::Error::from(io::ErrorKind::PermissionDenied)).is_err());
    }

    #[test]
    fn ok_passes_through_and_real_file_syncs() {
        assert!(classify_sync(Ok(()), Path::new("/x")).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let f = File::create(&p).unwrap();
        sync_best_effort(&f, &p).unwrap();
    }
}
