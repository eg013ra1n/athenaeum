//! The library path contract: how the web UI addresses files under a capture root.
//!
//! Every library route addresses a file as `(root_index, rel_path)`, where
//! `rel_path` is **always forward-slash separated** on the wire regardless of
//! host OS — the same convention the sync `rel_path` already uses. This module
//! is the single place that converts between that wire form and a real path on
//! disk, and it is the *only* containment guard: no route may join a
//! user-supplied string onto a capture root by hand.
//!
//! Two layers of defence, both required:
//!
//! 1. [`split_rel`] rejects hostile *syntax* up front — traversal (`..`), empty
//!    / relative-marker segments, native separators, drive-letter or
//!    alternate-data-stream colons, and NUL. A wire rel-path is a list of plain
//!    filename segments and nothing else.
//! 2. [`resolve_in_root`] canonicalizes **both** the root and the joined path and
//!    prefix-compares canonical against canonical. Syntax checks alone cannot see
//!    a symlink pointing out of the root; canonicalization resolves it, so the
//!    prefix check catches the escape. Comparing canonical-to-canonical also
//!    keeps Windows consistent (both sides carry the `\\?\C:\…` / `\\?\UNC\…`
//!    verbatim prefix) and absorbs macOS's `/var` → `/private/var` symlink.
//!
//! [`to_wire_rel`] is the inverse, for building listing and status payloads.
//!
//! Errors are [`anyhow`] with **stable message prefixes** that callers may match
//! on to pick an HTTP status: `"invalid path segment"`, `"path escapes root"`,
//! `"not found"`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Validate a wire rel-path and split it into plain filename segments.
///
/// The empty string is the root itself and yields an empty segment list.
/// Everything else must be a `/`-joined list of segments, each of which is a
/// plain filename: no `.`, no `..`, no empty segment (so no leading `/`, no
/// trailing `/`, no `//`), no backslash, no colon, no NUL.
///
/// Rejecting `\` and `:` costs a few legitimate POSIX filenames, which is the
/// intended trade: it means a wire path can never smuggle a Windows separator,
/// a drive letter (`C:/x`), or an alternate-data-stream suffix (`f.fits:evil`)
/// past a host that would honour it.
pub fn split_rel(rel: &str) -> Result<Vec<String>> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    if rel.starts_with('/') {
        bail!("invalid path segment: absolute");
    }
    let mut out = Vec::new();
    for seg in rel.split('/') {
        if seg.is_empty()
            || seg == "."
            || seg == ".."
            || seg.contains('\\')
            || seg.contains(':')
            || seg.contains('\0')
        {
            bail!("invalid path segment: {seg:?}");
        }
        out.push(seg.to_string());
    }
    Ok(out)
}

/// Resolve a wire rel-path inside `root`, guaranteeing the result stays inside it.
///
/// Returns the **canonical** absolute path of an existing file or directory.
/// The target must exist — canonicalization is what resolves symlinks, and
/// resolving them is the guard. Callers that create new paths must not use this.
///
/// Fails with `"invalid path segment: …"` on hostile syntax, `"not found: …"`
/// when the target does not exist, and `"path escapes root: …"` when the
/// canonical target falls outside the canonical root (the symlink case).
pub fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf> {
    let segs = split_rel(rel)?;
    let mut joined = root.to_path_buf();
    for s in &segs {
        joined.push(s);
    }
    let canon_root = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalize root {}", root.display()))?;
    let canon = std::fs::canonicalize(&joined)
        .with_context(|| format!("not found: {}", joined.display()))?;
    // Component-wise prefix check (not a string prefix): `/cap2` never counts as
    // being inside `/cap`.
    if !canon.starts_with(&canon_root) {
        bail!("path escapes root: {rel:?}");
    }
    Ok(canon)
}

/// Build the wire rel-path of `abs` relative to `root`, or `None` if `abs` is
/// not inside `root` (or either path cannot be canonicalized).
///
/// The inverse of [`resolve_in_root`]: components are joined with `/` so the
/// result is host-independent. The root itself yields `Some("")`.
///
/// A component that is not valid UTF-8 is rendered lossily, so its wire path
/// will not resolve back — a later [`resolve_in_root`] on it fails `"not found"`
/// rather than reaching the wrong file. Listings may therefore show such a file
/// while actions on it fail; a fully non-ASCII-safe listing is out of scope here.
pub fn to_wire_rel(root: &Path, abs: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).ok()?;
    let abs = std::fs::canonicalize(abs).ok()?;
    let rel = abs.strip_prefix(&root).ok()?;
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_rel_accepts_plain_segments() {
        assert_eq!(
            split_rel("M31/2026-07-01/light_0001.fits").unwrap(),
            vec!["M31", "2026-07-01", "light_0001.fits"]
        );
        assert_eq!(split_rel("").unwrap(), Vec::<String>::new()); // root itself
    }

    #[test]
    fn split_rel_rejects_hostile_segments() {
        for bad in [
            "..", "a/../b", ".", "a/./b", "a//b", "a\\b", "C:/x", "a\0b", "/abs",
        ] {
            assert!(split_rel(bad).is_err(), "must reject {bad:?}");
        }
    }

    /// Trailing separators and ADS-style suffixes are the two easy misses.
    #[test]
    fn split_rel_rejects_trailing_slash_and_stream_suffix() {
        for bad in ["M31/", "M31/a.fits:evil", "a/b/.."] {
            assert!(split_rel(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn resolve_in_root_stays_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(root.join("M31")).unwrap();
        std::fs::write(root.join("M31/a.fits"), b"x").unwrap();
        let p = resolve_in_root(&root, "M31/a.fits").unwrap();
        assert!(p.ends_with("a.fits"));
        assert!(resolve_in_root(&root, "../outside").is_err());
    }

    #[test]
    fn resolve_in_root_accepts_the_root_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            resolve_in_root(&root, "").unwrap(),
            std::fs::canonicalize(&root).unwrap()
        );
    }

    /// A symlink to a sibling whose name merely *starts with* the root's name
    /// must still be rejected — the check is component-wise, not string-prefix.
    #[cfg(unix)]
    #[test]
    fn resolve_in_root_rejects_sibling_name_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();
        let sibling = tmp.path().join("cap2");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("f.fits"), b"x").unwrap();
        std::os::unix::fs::symlink(&sibling, root.join("link")).unwrap();
        assert!(resolve_in_root(&root, "link/f.fits").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_in_root_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("secret");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.fits"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(
            resolve_in_root(&root, "link/f.fits").is_err(),
            "canonical prefix check must catch the escape"
        );
    }

    /// The three prefixes are a cross-task contract — later routes map them to
    /// HTTP statuses, so a reword here is a breaking change.
    #[test]
    fn error_message_prefixes_are_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();

        let bad_syntax = resolve_in_root(&root, "..").unwrap_err().to_string();
        assert!(
            bad_syntax.starts_with("invalid path segment"),
            "got {bad_syntax:?}"
        );

        let missing = resolve_in_root(&root, "nope.fits").unwrap_err().to_string();
        assert!(missing.starts_with("not found"), "got {missing:?}");

        #[cfg(unix)]
        {
            let outside = tmp.path().join("secret");
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            let escape = resolve_in_root(&root, "link").unwrap_err().to_string();
            assert!(escape.starts_with("path escapes root"), "got {escape:?}");
        }
    }

    #[test]
    fn to_wire_rel_roundtrips_with_forward_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(root.join("M31/sub")).unwrap();
        std::fs::write(root.join("M31/sub/a.fits"), b"x").unwrap();
        let abs = resolve_in_root(&root, "M31/sub/a.fits").unwrap();
        assert_eq!(to_wire_rel(&root, &abs).unwrap(), "M31/sub/a.fits");
    }

    #[test]
    fn to_wire_rel_rejects_outside_and_maps_root_to_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("secret");
        std::fs::create_dir_all(&outside).unwrap();

        assert_eq!(to_wire_rel(&root, &root).unwrap(), "");
        assert!(to_wire_rel(&root, &outside).is_none());
        assert!(to_wire_rel(&root, &tmp.path().join("gone")).is_none());
    }
}
