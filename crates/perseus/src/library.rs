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
//! `"not found"`, `"canonicalize root"` (the configured root itself is offline —
//! a `502`, not a client error) and, from [`list_directory`], `"not a directory"`.
//!
//! [`list_directory`] builds on this contract: it turns one
//! `(root_index, rel_path)` into a single-directory [`LibraryListing`] whose
//! files carry the derived per-file [`FileStatus`].

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::Config;

mod delete;
mod listing;
pub use delete::{
    delete_perform, delete_preview, plan_deletion, DeleteContext, DeleteOutcomeDto,
    DeleteOutcomeItem, DeletePlan, DeletePreviewItem, DeleteReport, DeleteSources,
    INTERNAL_PATH_REASON,
};
pub use listing::{
    list_directory, retention_fate, FileStatus, LibraryEntry, LibraryListing, StatusSources,
};

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

/// One item of a browser selection: a file or a whole directory, addressed in
/// the wire form every library route uses.
///
/// `dir` is the client telling us which it *meant*; the server verifies it
/// against the filesystem rather than guessing, so a selection built against a
/// stale listing fails loudly (`"not a file"` / `"not a directory"`) instead of
/// silently sending something else.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendItem {
    /// Index into [`Config::capture_dirs_resolved`]; `0` (the only root on a
    /// single-root node) when omitted.
    #[serde(default)]
    pub root: usize,
    /// Forward-slash wire rel-path inside that root; `""` is the root itself.
    pub rel: String,
    /// `true` → expand recursively (see [`expand_selection`]).
    #[serde(default)]
    pub dir: bool,
}

/// Expand a browser selection into the `(capture_dir, file)` pairs the send
/// pipeline speaks — the same pair shape the watcher feeds the batcher.
///
/// Every item passes the T1 guard ([`resolve_in_root`]), so the whole expansion
/// carries its stable error prefixes (`"invalid path segment"`, `"not found"`,
/// `"path escapes root"`, `"canonicalize root"`) plus two of its own for a
/// kind mismatch: `"not a file"` and `"not a directory"`.
///
/// **One failure is absorbed rather than propagated: an explicitly named
/// (`dir: false`) file that is no longer there.** A browser listing goes stale
/// the moment a capture is moved or a retention pass runs, and one such file must
/// not fail the operator's whole send — it is dropped with a `warn!` and the rest
/// of the selection proceeds (spec §1a, the eligible-subset stance). Every other
/// class still fails the request: hostile syntax, an escape and an offline root
/// are not "one file went missing", and a vanished **directory** is not either —
/// expanding a tree that is not there is a different selection, not a subset of
/// the asked-for one.
///
/// **The eligibility split is deliberate.** A `dir: true` item walks the subtree
/// and keeps only files with a capture extension
/// ([`is_eligible`](crate::watcher::is_eligible)) — picking a night's folder must
/// not sweep `.DS_Store`, logs or sidecars into a package. A file named
/// **explicitly** (`dir: false`) is sent whatever its extension: the operator
/// pointed at that exact file, and the receiver simply stores what arrives. (If
/// it turns out to be unparseable, [`build_batch_package`](crate::run::build_batch_package)
/// drops it with a `warn!` and the rest of the batch proceeds — an honest
/// eligible-subset outcome, not a silent one.)
///
/// Symlinks are never followed during the walk, so a symlinked subdirectory can
/// never contribute files from outside the root; the guard already refuses one
/// addressed directly.
///
/// The `capture_dir` half of every pair is the **canonical** root — the exact
/// spelling [`spawn_watcher`](crate::watcher::spawn_watcher) pairs its stable
/// files with — so [`BatcherHandle::remove_pending`](crate::batcher::BatcherHandle::remove_pending)
/// matches the pending entries by pair. Results are deduplicated (a directory
/// and a file inside it collapse) and deterministically ordered.
///
/// An empty result is **not** an error: the route turns "expanded to nothing"
/// into its own status, so an empty selection and an empty directory reach the
/// same answer.
///
/// Returns the pairs **and the number of named files skipped as vanished**, so
/// the route can report a partial send honestly (`skipped` in the send payload)
/// instead of leaving the operator to infer it from a smaller `enqueued` than
/// they picked. Only that one absorbed class is counted — every other drop is
/// either fatal here or happens later, at build time.
pub fn expand_selection(
    config: &Config,
    items: &[SendItem],
) -> Result<(Vec<(PathBuf, PathBuf)>, usize)> {
    let roots = config.capture_dirs_resolved();
    let mut out: BTreeSet<(PathBuf, PathBuf)> = BTreeSet::new();
    let mut skipped = 0usize;
    for item in items {
        let Some(root) = roots.get(item.root) else {
            bail!("not found: unknown capture root {}", item.root);
        };
        // Guard first: nothing below may see a path that has not been
        // canonicalized and prefix-checked against the root.
        let abs = match resolve_in_root(root, &item.rel) {
            Ok(abs) => abs,
            // The one absorbed failure (see the fn docs): a named file that
            // vanished under a stale listing. `"not found"` is the guard's
            // stable prefix for exactly that, so the match is on the class, not
            // on a substring of some path.
            Err(error) if !item.dir && error.to_string().starts_with("not found") => {
                // The bound error goes in the record, never dropped: `"not found"`
                // is only the guard's *wrapper*, and the cause underneath may be
                // an EACCES or an ELOOP rather than a real absence. Alternate
                // Display (`{:#}`) so the whole chain is visible, not just the
                // wrapper we matched on.
                let chain = format!("{error:#}");
                tracing::warn!(
                    error = %chain,
                    rel = %item.rel,
                    root_index = item.root,
                    "selected file vanished — skipped"
                );
                skipped += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let canon_root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalize root {}", root.display()))?;
        if item.dir {
            if !abs.is_dir() {
                bail!("not a directory: {:?}", item.rel);
            }
            for entry in walkdir::WalkDir::new(&abs).sort_by_file_name() {
                match entry {
                    Ok(e) if e.file_type().is_file() && crate::watcher::is_eligible(e.path()) => {
                        out.insert((canon_root.clone(), e.path().to_path_buf()));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        // One unreadable entry must not fail the whole selection
                        // (the same eligible-subset stance the batcher takes).
                        tracing::warn!(
                            %error,
                            path = %abs.display(),
                            "library send: skipped an unreadable entry while expanding a directory"
                        );
                    }
                }
            }
        } else {
            if !abs.is_file() {
                bail!("not a file: {:?}", item.rel);
            }
            out.insert((canon_root, abs));
        }
    }
    Ok((out.into_iter().collect(), skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config over a single capture root inside a fresh temp tree. Returns the
    /// tempdir guard (kept alive by the caller), the config, and the root.
    fn test_config() -> (tempfile::TempDir, Config, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let toml = format!(
            "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"manual\"\n\
             [retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            cap.display(),
            data.display()
        );
        let config = Config::from_toml_str(&toml).unwrap();
        (tmp, config, cap)
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        std::fs::canonicalize(&p).unwrap()
    }

    fn item(root: usize, rel: &str, dir: bool) -> SendItem {
        SendItem {
            root,
            rel: rel.to_string(),
            dir,
        }
    }

    /// Run `f` with a WARN-level subscriber attached to THIS thread and return
    /// its result plus everything that was logged.
    ///
    /// Thread-local ([`tracing::subscriber::with_default`]) rather than global:
    /// only one global subscriber may exist per test binary, and the code under
    /// test here is plain synchronous code running on the calling thread.
    fn capture_warnings<T>(f: impl FnOnce() -> T) -> (T, String) {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl tracing_subscriber::fmt::MakeWriter<'_> for Buf {
            type Writer = Buf;
            fn make_writer(&self) -> Buf {
                self.clone()
            }
        }

        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(buf.clone())
            .finish();
        let out = tracing::subscriber::with_default(subscriber, f);
        let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        (out, logged)
    }

    /// A file that vanished between the listing and the send is dropped from the
    /// selection and the rest still ships (spec §1a) — one deleted frame under a
    /// stale browser listing must not fail the operator's whole send. The skip is
    /// logged, never silent.
    #[test]
    fn expand_selection_skips_a_vanished_file_and_keeps_the_rest() {
        let (_tmp, config, cap) = test_config();
        let a = write(&cap, "a.fits", b"x");

        let ((files, skipped), logged) = capture_warnings(|| {
            expand_selection(
                &config,
                &[item(0, "a.fits", false), item(0, "gone.fits", false)],
            )
            .expect("a vanished file is skipped, not fatal")
        });

        assert_eq!(
            files.iter().map(|(_, f)| f.clone()).collect::<Vec<_>>(),
            vec![a],
            "the surviving file still ships"
        );
        assert_eq!(
            skipped, 1,
            "the drop is counted, so the route can report it"
        );
        assert!(
            logged.contains("selected file vanished") && logged.contains("gone.fits"),
            "the skip must be logged with the file it dropped, got {logged:?}"
        );
        // The bound error is never swallowed: `"not found"` is a wrapper that can
        // hide an EACCES/ELOOP, so the cause has to be in the record.
        assert!(
            logged.contains("error="),
            "the skip must carry the error it absorbed, got {logged:?}"
        );
    }

    /// A vanished **directory** is NOT absorbed: expanding a tree that is not
    /// there is a different selection, not a subset of the asked-for one.
    #[test]
    fn expand_selection_still_fails_on_a_vanished_directory() {
        let (_tmp, config, _cap) = test_config();
        let err = expand_selection(&config, &[item(0, "gone", true)])
            .expect_err("a missing directory is fatal")
            .to_string();
        assert!(err.starts_with("not found"), "got {err:?}");
    }

    /// A `dir: true` item walks the whole subtree, files only, and keeps ONLY
    /// capture-eligible extensions — a browser directory pick must not sweep
    /// `.DS_Store` / logs / sidecars into a package.
    #[test]
    fn expand_selection_walks_a_directory_recursively_eligible_only() {
        let (_tmp, config, cap) = test_config();
        let b = write(&cap, "M31/b.fits", b"x");
        let c = write(&cap, "M31/sub/c.xisf", b"x");
        write(&cap, "M31/notes.txt", b"x");
        write(&cap, "M31/.DS_Store", b"x");
        write(&cap, "other.fits", b"x"); // outside the selected dir

        let (files, skipped) = expand_selection(&config, &[item(0, "M31", true)]).unwrap();
        let got: Vec<PathBuf> = files.iter().map(|(_, f)| f.clone()).collect();
        assert_eq!(got, vec![b, c], "recursive, files only, eligible only");
        assert_eq!(skipped, 0, "nothing vanished");
    }

    /// An explicitly named file is the operator's call: it ships even when its
    /// extension is not one the watcher would ever enqueue. (The build drops it
    /// with a `warn!` if it turns out to be unparseable — that is a different,
    /// honest failure.) Only *directory expansion* filters by eligibility.
    #[test]
    fn expand_selection_allows_an_explicitly_named_ineligible_file() {
        let (_tmp, config, cap) = test_config();
        let notes = write(&cap, "notes.txt", b"x");
        let (files, _) = expand_selection(&config, &[item(0, "notes.txt", false)]).unwrap();
        assert_eq!(
            files.iter().map(|(_, f)| f.clone()).collect::<Vec<_>>(),
            vec![notes]
        );
    }

    /// Every item runs through the T1 guard, and the two kind mismatches
    /// (`dir` on a file, file on a directory) are their own stable prefixes.
    ///
    /// A named file that is merely *missing* is deliberately absent from this
    /// matrix — that is the one absorbed class, pinned by
    /// [`expand_selection_skips_a_vanished_file_and_keeps_the_rest`].
    #[test]
    fn expand_selection_applies_the_guard_and_the_kind_check() {
        let (_tmp, config, cap) = test_config();
        write(&cap, "M31/b.fits", b"x");

        let cases: Vec<(SendItem, &str)> = vec![
            (item(0, "..", false), "invalid path segment"),
            (item(0, "a\\b.fits", false), "invalid path segment"),
            (item(0, "nope", true), "not found"),
            (item(0, "M31", false), "not a file"),
            (item(0, "M31/b.fits", true), "not a directory"),
            (item(7, "", true), "not found"),
        ];
        for (it, prefix) in cases {
            let err = expand_selection(&config, &[it.clone()])
                .expect_err(&format!("{it:?} must fail"))
                .to_string();
            assert!(err.starts_with(prefix), "{it:?} → {err:?}");
        }
    }

    /// The `capture_dir` half of every pair is the CANONICAL root — the exact
    /// spelling the watcher pairs its stable files with, so
    /// [`BatcherHandle::remove_pending`](crate::batcher::BatcherHandle::remove_pending)
    /// matches by pair. Overlapping items (a directory and a file inside it)
    /// collapse to one entry.
    #[test]
    fn expand_selection_dedupes_and_pairs_with_the_canonical_root() {
        let (_tmp, config, cap) = test_config();
        let b = write(&cap, "M31/b.fits", b"x");
        let (files, _) = expand_selection(
            &config,
            &[item(0, "M31", true), item(0, "M31/b.fits", false)],
        )
        .unwrap();
        assert_eq!(
            files,
            vec![(std::fs::canonicalize(&cap).unwrap(), b)],
            "one pair, canonical root"
        );
    }

    /// An empty selection is not an error here — the route turns "expanded to
    /// nothing" into its own `422`, and an empty directory must reach that same
    /// answer rather than a different one.
    #[test]
    fn expand_selection_of_nothing_is_empty_not_an_error() {
        let (_tmp, config, cap) = test_config();
        std::fs::create_dir_all(cap.join("empty")).unwrap();
        assert_eq!(expand_selection(&config, &[]).unwrap(), (Vec::new(), 0));
        assert_eq!(
            expand_selection(&config, &[item(0, "empty", true)]).unwrap(),
            (Vec::new(), 0)
        );
    }

    /// A symlinked subdirectory is never followed: its contents belong to
    /// another tree, and following it would package files from outside the root.
    #[cfg(unix)]
    #[test]
    fn expand_selection_does_not_follow_symlinks_out_of_the_root() {
        let (tmp, config, cap) = test_config();
        let outside = tmp.path().join("secret");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.fits"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, cap.join("link")).unwrap();
        write(&cap, "own.fits", b"x");

        // The whole root: the symlinked dir contributes nothing.
        let (files, _) = expand_selection(&config, &[item(0, "", true)]).unwrap();
        assert_eq!(files.len(), 1, "only the root's own file: {files:?}");
        // And addressing it directly is refused by the guard.
        assert!(expand_selection(&config, &[item(0, "link", true)]).is_err());
    }

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
