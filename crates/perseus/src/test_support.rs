//! Test-only helpers for building `perseus.toml` fixture text.
//!
//! # Why this module exists
//!
//! Every fixture in this crate hand-builds config TOML with a temp path spliced
//! into it. The obvious spelling is a TOML **basic** string:
//!
//! ```ignore
//! format!("data_dir = \"{}\"\n", dir.display())   // WRONG
//! ```
//!
//! That is correct on unix and silently broken on Windows: TOML basic strings
//! treat `\` as an escape introducer, so a Windows temp path
//! (`C:\Users\…\Temp\.tmpAbC`) makes the parser read `\U` as a truncated
//! `\UXXXXXXXX` and reject the whole file with "too few unicode value digits".
//! The fixture never parses, so the test never exercises what it claims to.
//!
//! This cost the 2026-09-07 Windows cycle 227 perseus failures from one cause —
//! including all 9 supervisor tests, which timed out waiting for an agent that
//! could never launch because its config would not parse.
//!
//! [`toml_path`] is the single seam that gets it right, and
//! `fixtures_never_splice_a_path_into_a_toml_basic_string` below keeps it that
//! way.

use std::path::Path;

/// Render `path` as a quoted TOML string value, safe to splice into fixture
/// TOML on every platform.
///
/// Delegates to `toml`'s own encoder rather than picking quotes by hand: a
/// single-quoted literal string fixes the backslash but breaks on an
/// apostrophe (`/Users/O'Brien`), and a hand-rolled choice between the two is
/// exactly the kind of thing that grows a third bug. This is also the encoder
/// production uses — `config_edit` writes paths through `toml_edit` — so a
/// fixture and a real config now agree on what a path looks like on disk.
///
/// Returns the quotes too, so call sites read
/// `format!("data_dir = {}", toml_path(p))` — the quoting belongs to the
/// encoder, not to the format string.
pub(crate) fn toml_path(path: impl AsRef<Path>) -> String {
    toml::Value::String(path.as_ref().display().to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this module exists to prevent, pinned as a fact about TOML rather
    /// than as a fact about Windows: the basic-string spelling is genuinely
    /// invalid TOML, on every platform, for any path containing a backslash.
    #[test]
    fn a_windows_path_in_a_toml_basic_string_is_invalid_toml() {
        let bad = format!(
            "data_dir = \"{}\"\n",
            Path::new(r"C:\Users\me\AppData\Local\Temp\.tmpAbC").display()
        );
        let err = toml::from_str::<toml::Value>(&bad)
            .expect_err("a basic string containing \\U must not parse");
        assert!(
            err.to_string().contains("unicode") || err.to_string().contains("escape"),
            "expected an escape-sequence complaint, got: {err}"
        );
    }

    /// The contract: a Windows-shaped path splices in and reads back byte-identical.
    ///
    /// Hard-coded rather than taken from `tempfile`, so this fails on macOS and
    /// Linux too — a Windows-only reproduction would leave the fix unverifiable
    /// on the machine it is written on.
    #[test]
    fn toml_path_round_trips_a_windows_path() {
        let raw = r"C:\Users\me\AppData\Local\Temp\.tmpAbC";
        let text = format!("data_dir = {}\n", toml_path(Path::new(raw)));

        let parsed: toml::Value = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("fixture TOML must parse, got {e} for: {text}"));

        assert_eq!(parsed["data_dir"].as_str().unwrap(), raw);
    }

    /// A literal string cannot contain a single quote, and `/Users/O'Brien` is
    /// an entirely ordinary home directory. The helper must not simply trade the
    /// backslash bug for an apostrophe bug.
    #[test]
    fn toml_path_round_trips_a_path_containing_an_apostrophe() {
        let raw = "/Users/O'Brien/Astro";
        let text = format!("data_dir = {}\n", toml_path(Path::new(raw)));

        let parsed: toml::Value = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("fixture TOML must parse, got {e} for: {text}"));

        assert_eq!(parsed["data_dir"].as_str().unwrap(), raw);
    }

    /// The nastiest shape: both. A Windows profile for the same person.
    #[test]
    fn toml_path_round_trips_a_windows_path_containing_an_apostrophe() {
        let raw = r"C:\Users\O'Brien\AppData\Local\Temp\.tmpAbC";
        let text = format!("data_dir = {}\n", toml_path(Path::new(raw)));

        let parsed: toml::Value = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("fixture TOML must parse, got {e} for: {text}"));

        assert_eq!(parsed["data_dir"].as_str().unwrap(), raw);
    }

    /// A unix path must keep working unchanged — the helper is used everywhere,
    /// so it may not regress the platform that was already green.
    #[test]
    fn toml_path_round_trips_a_unix_path() {
        let raw = "/var/folders/kx/T/.tmpAbC";
        let text = format!("data_dir = {}\n", toml_path(Path::new(raw)));

        let parsed: toml::Value = toml::from_str(&text).expect("fixture TOML must parse");

        assert_eq!(parsed["data_dir"].as_str().unwrap(), raw);
    }
}

/// Guards the whole crate against the construction [`toml_path`] exists to
/// replace: a path spliced into a TOML **basic** string.
///
/// Deliberately narrow. It walks only `crates/perseus/src` and only flags a
/// `format!` that contains BOTH an escaped quote and a `.display()` argument —
/// the exact shape that produced 227 Windows failures. A `format!` with quotes
/// but no path (`mode = \"{}\"` over a `&str`) is fine and stays fine, as is a
/// `.display()` with no quotes around it; the guard has no opinion on either.
///
/// The sibling guard in `athenaeum-core/src/test_support.rs` scans only that
/// crate's `src`, which is precisely why this class survived here unnoticed.
#[test]
fn fixtures_never_splice_a_path_into_a_toml_basic_string() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();

    for entry in walkdir::WalkDir::new(&src).into_iter().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This module is where the correct spelling lives, and its own tests
        // quote the wrong one on purpose to pin why.
        if path == src.join("test_support.rs") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();

        for (offset, _) in text.match_indices("format!(") {
            let body = balanced_call_body(&text, offset + "format!(".len());
            // An escaped quote plus a `.display()` in the same `format!`: a path
            // is being wrapped in a TOML basic string. Catches the assignment
            // form (`data_dir = \"{}\"`) and the array-element form
            // (`format!("\"{}\"", d.display())`) alike — the latter has no `=`
            // in it at all, which an assignment-shaped guard would miss.
            if body.contains("\\\"") && body.contains(".display()") {
                let line = text[..offset].lines().count();
                violations.push(format!(
                    "{}:{}",
                    path.strip_prefix(&src).unwrap().display(),
                    line + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "a path is being spliced into a TOML basic string — use \
         `test_support::toml_path()` instead, or TOML will read the backslashes \
         of a Windows path as escape sequences and the fixture will never \
         parse. Sites:\n  {}",
        violations.join("\n  ")
    );
}

/// The text between `start` and the `)` that closes the call opened before it,
/// counting nested parens. Returns the rest of the file if unbalanced — a
/// truncated read would only ever hide a violation, never invent one.
fn balanced_call_body(text: &str, start: usize) -> &str {
    let mut depth = 1usize;
    for (i, c) in text[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[start..start + i];
                }
            }
            _ => {}
        }
    }
    &text[start..]
}
