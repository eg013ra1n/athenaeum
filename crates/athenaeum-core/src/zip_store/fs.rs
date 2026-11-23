//! File-system helpers shared by `zip_store` and `archive`.

use std::io;
use std::path::Path;

// The one copy lives in starloom; archive, zip and export writes reach it through this path.
pub(crate) use starloom::fs_util::sync_best_effort;

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
