//! File-system helpers shared by `zip_store` and `archive`.

use std::fs::File;
use std::io;
use std::path::Path;

/// True when `e` says the file system does not implement the sync that was asked for (some SMB/NFS/FUSE
/// mounts answer `ENOTSUP`/`EOPNOTSUPP` (45 and 102 on macOS), others `EINVAL`).
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

/// The plain `fsync(2)` that a share still honours when `F_FULLFSYNC` (what `File::sync_all` is on macOS)
/// is refused. Off macOS `sync_all` already is a plain fsync, so there is nothing weaker to try.
fn plain_fsync(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor belongs to `file`, which outlives the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// The classification and the fallback of [`sync_best_effort`], with both sync calls injected so a test can
/// drive them. `full` is tried first; when it says "not supported", `plain` is tried once; a "not supported"
/// from that too leaves the file to the file system. Any other error, from either call, is returned. One
/// `debug!` is logged when the full sync was refused.
fn sync_with(
    full: impl FnOnce() -> io::Result<()>,
    plain: impl FnOnce() -> io::Result<()>,
    path: &Path,
) -> io::Result<()> {
    let refused = match full() {
        Err(e) if fsync_unsupported(&e) => e,
        other => return other,
    };
    match plain() {
        Ok(()) => {
            tracing::debug!(path = %path.display(), error = %refused, "full sync not supported; plain sync used");
            Ok(())
        }
        Err(e) if fsync_unsupported(&e) => {
            tracing::debug!(path = %path.display(), error = %refused, "full sync not supported; file left to the file system");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// `file.sync_all()` that tolerates file systems without a full sync (network shares): a cache or output
/// file on an SMB share is written without it rather than failing. `path` is for the log only.
///
/// On macOS `sync_all` issues `F_FULLFSYNC`, which an SMB mount refuses with `ENOTSUP` while a plain
/// `fsync(2)` still flushes to the server, so the plain call is tried before the sync is given up. Every
/// other error (EIO, ENOSPC, ...) is returned.
pub(crate) fn sync_best_effort(file: &File, path: &Path) -> io::Result<()> {
    sync_with(|| file.sync_all(), || plain_fsync(file), path)
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

    /// A full sync that fails with `e`, with a plain sync that must not run.
    fn classify(e: io::Error) -> io::Result<()> {
        sync_with(|| Err(e), || Ok(()), Path::new("/x"))
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
        assert!(sync_with(|| Ok(()), || panic!("plain sync after a good full sync"), Path::new("/x")).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let f = File::create(&p).unwrap();
        sync_best_effort(&f, &p).unwrap();
    }

    /// The two raw errors a share answers on macOS (45 = ENOTSUP, 102 = EOPNOTSUPP) and the portable kind
    /// are "not supported"; a real I/O failure and a permission or lookup error are not.
    #[cfg(unix)]
    #[test]
    fn classification_of_sync_errors() {
        assert!(fsync_unsupported(&io::Error::from(io::ErrorKind::Unsupported)));
        assert!(fsync_unsupported(&io::Error::from_raw_os_error(libc::ENOTSUP)));
        assert!(fsync_unsupported(&io::Error::from_raw_os_error(libc::EOPNOTSUPP)));
        #[cfg(target_os = "macos")]
        {
            assert!(fsync_unsupported(&io::Error::from_raw_os_error(45)));
            assert!(fsync_unsupported(&io::Error::from_raw_os_error(102)));
        }
        assert!(!fsync_unsupported(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(!fsync_unsupported(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!fsync_unsupported(&io::Error::from_raw_os_error(libc::EIO)));
    }

    #[cfg(unix)]
    #[test]
    fn the_plain_sync_runs_once_when_the_full_sync_is_unsupported() {
        let mut plain_calls = 0;
        let r = sync_with(
            || Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
            || {
                plain_calls += 1;
                Ok(())
            },
            Path::new("/x"),
        );
        assert!(r.is_ok());
        assert_eq!(plain_calls, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_plain_sync_that_is_unsupported_too_leaves_the_file_unsynced() {
        let r = sync_with(
            || Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
            || Err(io::Error::from(io::ErrorKind::Unsupported)),
            Path::new("/x"),
        );
        assert!(r.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn an_io_error_is_returned_unchanged_and_never_retried() {
        let mut plain_calls = 0;
        let e = sync_with(
            || Err(io::Error::from_raw_os_error(libc::EIO)),
            || {
                plain_calls += 1;
                Ok(())
            },
            Path::new("/x"),
        )
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EIO));
        assert_eq!(plain_calls, 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_io_error_from_the_plain_sync_is_returned() {
        let e = sync_with(
            || Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
            || Err(io::Error::from_raw_os_error(libc::EIO)),
            Path::new("/x"),
        )
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EIO));
    }
}
