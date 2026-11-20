//! Where the assistant bridge program (`athenaeum-mcp`) is, and how the desktop keeps a copy of it that survives the
//! installer (MCP plan, Task 8, decision D2).
//!
//! On macOS and a `.deb` the bridge sits next to the app's executable, and the configuration an assistant copies
//! names that path. An AppImage's executable lives in a mount that is gone when the app closes, and on Windows an
//! assistant that holds the bridge open would block an update: both keep a copy in the app-data folder
//! (`<app-data>/bin/`), refreshed when the app's version changes, and the configuration names the copy.

use std::io;
use std::path::{Path, PathBuf};

/// The bridge program's name without its platform suffix.
const BRIDGE_STEM: &str = "athenaeum-mcp";

/// Beside the copy: the app version it was copied from.
const VERSION_FILE: &str = "athenaeum-mcp.version";

/// The bridge's file name on this platform (`athenaeum-mcp` or `athenaeum-mcp.exe`).
fn bridge_file_name() -> String {
    format!("{BRIDGE_STEM}{}", std::env::consts::EXE_SUFFIX)
}

/// Where the bridge is: next to the app's executable, or (`copy_to_app_data`: AppImage or Windows) the copy in
/// `<app_data>/bin/`.
pub(crate) fn bridge_path_for(exe: &Path, copy_to_app_data: bool, app_data: &Path) -> PathBuf {
    if copy_to_app_data {
        return app_data.join("bin").join(bridge_file_name());
    }
    match exe.parent() {
        Some(dir) => dir.join(bridge_file_name()),
        None => PathBuf::from(bridge_file_name()),
    }
}

/// This platform keeps its own copy of the bridge in the app-data folder: an AppImage (its mount goes away with the
/// app) or Windows (a client holding the installed exe would block an update).
pub(crate) fn copies_to_app_data() -> bool {
    cfg!(windows) || std::env::var_os("APPIMAGE").is_some_and(|v| !v.is_empty())
}

/// What [`refresh_bridge_copy`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    /// The copy is of this app version already.
    Current,
    /// A new copy replaced the old one.
    Refreshed,
    /// The old copy (if any) stays, for the reason given; the next start tries again.
    Kept(String),
}

fn keep(reason: String) -> CopyOutcome {
    tracing::warn!(reason = %reason, "mcp bridge copy kept; it is refreshed at the next start");
    CopyOutcome::Kept(reason)
}

/// Makes `<dst_dir>/athenaeum-mcp[.exe]` a copy of `src` when the `athenaeum-mcp.version` file beside it does not
/// say `app_version`: the bridge is copied to a temporary name, then `rename`d over the copy. A rename that fails (a
/// client holds the executable, Windows) leaves the old copy and its version file as they were, removes the temporary
/// file, and is tried again by the next call. The channel handshake is versioned, so a bridge one version behind
/// still works.
pub(crate) fn refresh_bridge_copy(
    src: &Path,
    dst_dir: &Path,
    app_version: &str,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> CopyOutcome {
    let dst = dst_dir.join(bridge_file_name());
    let version_file = dst_dir.join(VERSION_FILE);
    let copied_version = std::fs::read_to_string(&version_file).ok();
    if dst.is_file() && copied_version.as_deref().map(str::trim) == Some(app_version) {
        return CopyOutcome::Current;
    }
    if let Err(e) = std::fs::create_dir_all(dst_dir) {
        tracing::warn!(error = %e, path = %dst_dir.display(), "mcp bridge folder could not be created");
        return keep(format!("the bridge folder could not be created: {e}"));
    }
    let temp = dst_dir.join(format!("{BRIDGE_STEM}.new{}", std::env::consts::EXE_SUFFIX));
    if let Err(e) = std::fs::copy(src, &temp) {
        tracing::warn!(error = %e, path = %src.display(), "mcp bridge could not be copied");
        remove_temp(&temp);
        return keep(format!("the bridge program could not be copied from {}: {e}", src.display()));
    }
    if let Err(e) = rename(&temp, &dst) {
        tracing::warn!(error = %e, path = %dst.display(), "mcp bridge copy could not be replaced");
        remove_temp(&temp);
        return keep(format!("the bridge copy is in use and could not be replaced: {e}"));
    }
    if let Err(e) = std::fs::write(&version_file, app_version) {
        // The new copy is in place; without its version the next start copies once more, which is harmless.
        tracing::warn!(error = %e, path = %version_file.display(), "mcp bridge version could not be written");
    }
    tracing::info!(path = %dst.display(), version = app_version, "mcp bridge copy refreshed");
    CopyOutcome::Refreshed
}

fn remove_temp(temp: &Path) {
    if let Err(e) = std::fs::remove_file(temp) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %temp.display(), "mcp bridge temporary copy could not be removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn name() -> String {
        format!("athenaeum-mcp{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn bridge_path_next_to_the_exe() {
        let exe = Path::new("app").join("bin").join("athenaeum");
        let data = Path::new("data");
        assert_eq!(bridge_path_for(&exe, false, data), Path::new("app").join("bin").join(name()));
        assert_eq!(bridge_path_for(&exe, true, data), data.join("bin").join(name()));
    }

    fn source(dir: &Path, bytes: &[u8]) -> PathBuf {
        let src = dir.join("installed").join(name());
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, bytes).unwrap();
        src
    }

    #[test]
    fn bridge_copy_refreshes_when_the_version_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let src = source(tmp.path(), b"bridge-v1");
        let dst_dir = tmp.path().join("data").join("bin");

        // No copy yet: made.
        assert_eq!(refresh_bridge_copy(&src, &dst_dir, "0.7.1", |a, b| std::fs::rename(a, b)), CopyOutcome::Refreshed);
        assert_eq!(std::fs::read(dst_dir.join(name())).unwrap(), b"bridge-v1");
        assert_eq!(std::fs::read_to_string(dst_dir.join(VERSION_FILE)).unwrap(), "0.7.1");

        // Same version: left alone, even though the source changed.
        std::fs::write(&src, b"bridge-v2").unwrap();
        assert_eq!(refresh_bridge_copy(&src, &dst_dir, "0.7.1", |a, b| std::fs::rename(a, b)), CopyOutcome::Current);
        assert_eq!(std::fs::read(dst_dir.join(name())).unwrap(), b"bridge-v1");

        // A new app version: replaced, and no temporary file is left behind.
        assert_eq!(refresh_bridge_copy(&src, &dst_dir, "0.7.2", |a, b| std::fs::rename(a, b)), CopyOutcome::Refreshed);
        assert_eq!(std::fs::read(dst_dir.join(name())).unwrap(), b"bridge-v2");
        assert_eq!(std::fs::read_to_string(dst_dir.join(VERSION_FILE)).unwrap(), "0.7.2");
        let left: Vec<_> = std::fs::read_dir(&dst_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), 2, "only the copy and its version file remain: {left:?}");
    }

    #[test]
    fn locked_copy_is_kept_and_refreshed_on_a_later_start() {
        let tmp = tempfile::tempdir().unwrap();
        let src = source(tmp.path(), b"bridge-v1");
        let dst_dir = tmp.path().join("data").join("bin");
        assert_eq!(refresh_bridge_copy(&src, &dst_dir, "0.7.1", |a, b| std::fs::rename(a, b)), CopyOutcome::Refreshed);
        std::fs::write(&src, b"bridge-v2").unwrap();

        // The rename fails once (a client holds the executable).
        let failed_once = Cell::new(false);
        let locked = |a: &Path, b: &Path| {
            if failed_once.replace(true) {
                std::fs::rename(a, b)
            } else {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"))
            }
        };
        let outcome = refresh_bridge_copy(&src, &dst_dir, "0.7.2", &locked);
        assert!(matches!(outcome, CopyOutcome::Kept(_)), "{outcome:?}");
        assert_eq!(std::fs::read(dst_dir.join(name())).unwrap(), b"bridge-v1", "the old copy stays");
        assert_eq!(std::fs::read_to_string(dst_dir.join(VERSION_FILE)).unwrap(), "0.7.1", "and so does its version");
        assert!(!dst_dir.join(format!("athenaeum-mcp.new{}", std::env::consts::EXE_SUFFIX)).exists(), "the temporary copy is removed");

        // The next start: the lock is gone.
        assert_eq!(refresh_bridge_copy(&src, &dst_dir, "0.7.2", &locked), CopyOutcome::Refreshed);
        assert_eq!(std::fs::read(dst_dir.join(name())).unwrap(), b"bridge-v2");
        assert_eq!(std::fs::read_to_string(dst_dir.join(VERSION_FILE)).unwrap(), "0.7.2");
    }

    #[test]
    fn missing_source_keeps_what_is_there() {
        let tmp = tempfile::tempdir().unwrap();
        let dst_dir = tmp.path().join("data").join("bin");
        let outcome = refresh_bridge_copy(&tmp.path().join("nowhere").join(name()), &dst_dir, "0.7.1", |a, b| std::fs::rename(a, b));
        assert!(matches!(outcome, CopyOutcome::Kept(_)), "{outcome:?}");
        assert!(!dst_dir.join(name()).exists());
    }
}
