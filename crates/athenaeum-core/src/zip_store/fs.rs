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
pub(crate) fn sync_best_effort(file: &File, path: &Path) -> io::Result<()> {
    classify_sync(file.sync_all(), path)
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
