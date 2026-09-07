//! Shared test fixtures.
//!
//! Path fixtures in particular. A test that seeds the catalog must seed what
//! production would have written, and production normalises at the write
//! boundary — `normalize_path(canonicalize(...))`, see the invariant in
//! `docs/superpowers/specs/2026-09-07-windows-test-failures-design.md` §3.
//! `canonicalize` alone returns a `\\?\` verbatim path on Windows, which neither
//! compares equal to the stored spelling nor accepts a forward slash as a
//! separator.

use std::path::PathBuf;

/// A temporary directory, plus the spelling production would have stored for it.
///
/// Bind the `TempDir`: dropping it deletes the directory out from under the path.
///
/// ```ignore
/// let (_tmp, base) = crate::test_support::canonical_tempdir();
/// ```
pub(crate) fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize tempdir");
    let base = crate::api::scan_roots::normalize_path(&canonical);
    (dir, base)
}

/// The spelling production would have stored for an existing directory.
///
/// For a test that already holds a `TempDir` — from `test_ctx()`, say — and must
/// not create a second one.
pub(crate) fn canonical_path(path: &std::path::Path) -> PathBuf {
    let canonical = path.canonicalize().expect("canonicalize path");
    crate::api::scan_roots::normalize_path(&canonical)
}

/// No fixture may seed a raw `canonicalize()` result.
///
/// This is a drift guard in the same genre as
/// `sync::receiver::tests::every_terminal_writer_announces_or_is_a_named_exemption`.
/// Without it the next fixture repeats the mistake and only a Windows runner
/// notices — which is exactly how 39 tests accumulated unmeasured.
#[test]
fn fixtures_never_seed_a_raw_canonicalized_path() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(&src).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file defines the sanctioned helper, so it necessarily contains
        // the call the guard forbids everywhere else.
        if path.file_name().and_then(|n| n.to_str()) == Some("test_support.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("read source");
        // Drop comment lines so prose about the rule never trips it, then drop
        // all whitespace so a multi-line builder chain cannot hide.
        let squashed: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        if squashed.contains("canonicalize().unwrap()")
            || squashed.contains("canonicalize().expect(")
        {
            offenders.push(
                path.strip_prefix(&src)
                    .unwrap_or(path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "these files seed a raw `canonicalize()` result: {offenders:?}\n\
         On Windows that is a `\\\\?\\` verbatim path. It never compares equal to \
         the normalised spelling production stores, and it takes a forward slash \
         as a filename character rather than a separator (error 123, \
         InvalidFilename). Use `crate::test_support::canonical_tempdir()`."
    );
}
