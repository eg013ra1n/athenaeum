//! The shim guard (starloom phase 1, review focus 3): a core module that shadows a starloom item to add an
//! Athenaeum fact re-exports the rest of starloom's module by name. A glob would let a fact-less starloom entry point
//! (a writer without the creator, a measure without the gain, a flat-norm reader without the cards) resolve under an
//! old `athenaeum_core::` path, so each such module lists its names and never ends a `pub use starloom::…` in `::*`.

use std::path::PathBuf;

/// One file and the starloom names its `pub use starloom::…;` statements must never carry.
struct Shim {
    file: &'static str,
    excluded: &'static [&'static str],
}

const SHIMS: &[Shim] = &[
    Shim {
        file: "stacking/measure.rs",
        excluded: &["measure_frame_full", "measure_frame_pass1_gain"],
    },
    Shim {
        file: "calibration_library/light_cal.rs",
        excluded: &[
            "FlatCardReader",
            "NoFlatCards",
            "calibrate_light_compute_with_cards",
            "calibrate_light_compute_budgeted",
            "resolve_flat_norm_divisor_with",
            "flat_norm_constant_with",
        ],
    },
    Shim {
        file: "stacking/ln/mod.rs",
        excluded: &["write_reference", "write_reference_saturation"],
    },
    Shim {
        file: "stacking/ln/reference.rs",
        excluded: &[],
    },
    Shim {
        file: "fits_writer/mod.rs",
        excluded: &["write_xisf_f32_full", "write_xisf_f32_to_full", "write_image_f32_full"],
    },
];

/// `line` without its `//` comment (a doc comment is all comment).
fn strip_comment(line: &str) -> &str {
    line.find("//").map_or(line, |at| &line[..at])
}

/// Every statement of `src` that starts with `prefix`, from the prefix to its first `;` (comments stripped, lines
/// joined with a space). A statement with no `;` runs to the end of the file.
fn statements_starting_with(src: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let t = strip_comment(line).trim();
        if !t.starts_with(prefix) {
            continue;
        }
        let mut stmt = t.to_string();
        while !stmt.contains(';') {
            match lines.next() {
                Some(next) => {
                    stmt.push(' ');
                    stmt.push_str(strip_comment(next).trim());
                }
                None => break,
            }
        }
        if let Some(end) = stmt.find(';') {
            stmt.truncate(end + 1);
        }
        out.push(stmt);
    }
    out
}

/// The names the file binds with `use starloom::… as <alias>;`.
fn starloom_aliases(src: &str) -> Vec<String> {
    statements_starting_with(src, "use starloom::")
        .iter()
        .filter_map(|stmt| {
            let body = stmt.trim_end_matches(';').trim_end();
            if body.contains('{') {
                return None;
            }
            let (_, alias) = body.rsplit_once(" as ")?;
            Some(alias.trim().to_string())
        })
        .collect()
}

/// Every statement of `src` that re-exports starloom: `pub use starloom::…;` and `pub use <alias>::…;` for each alias
/// the file binds to a starloom path.
fn starloom_reexports(src: &str) -> Vec<String> {
    let mut out = statements_starting_with(src, "pub use starloom::");
    for alias in starloom_aliases(src) {
        out.extend(statements_starting_with(src, &format!("pub use {alias}::")));
    }
    out
}

/// Why `stmt` may not stand in a shim that excludes `excluded`, or `None` when it is clean: any `*` is a glob (plain,
/// braced, or after a comma), and an excluded name never appears.
fn violation(stmt: &str, excluded: &[&str]) -> Option<String> {
    if stmt.contains('*') {
        return Some("a module that shadows a starloom item re-exports it by name, never by glob".to_string());
    }
    excluded.iter().find(|word| has_word(stmt, word)).map(|word| {
        format!("`{word}` is a starloom form without the Athenaeum fact and must not be re-exported")
    })
}

/// Whether `word` occurs in `text` as a whole identifier.
fn has_word(text: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before_ok = text[..start].chars().next_back().map_or(true, |c| !is_ident(c));
        let after_ok = text[end..].chars().next().map_or(true, |c| !is_ident(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

#[test]
fn fact_bearing_shims_reexport_by_name() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    for shim in SHIMS {
        let path = src_dir.join(shim.file);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let statements = starloom_reexports(&src);
        assert!(!statements.is_empty(), "{}: no `pub use starloom::…;` statement found", shim.file);
        for stmt in &statements {
            if let Some(why) = violation(stmt, shim.excluded) {
                panic!("{}: {why} (starloom phase 1): `{stmt}`", shim.file);
            }
        }
    }
}

#[test]
fn guard_flags_a_trailing_comment_glob() {
    let src = "pub use starloom::stacking::measure::*; // everything\n";
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use starloom::stacking::measure::*;".to_string()]);
    assert!(violation(&found[0], &[]).is_some());
}

#[test]
fn guard_flags_braced_globs() {
    for src in ["pub use starloom::stacking::{measure::*};\n", "pub use starloom::stacking::measure::{self, *};\n"] {
        let found = starloom_reexports(src);
        assert_eq!(found.len(), 1, "{src}");
        assert!(violation(&found[0], &[]).is_some(), "{src}");
    }
}

#[test]
fn guard_flags_an_alias_glob() {
    let src = "use starloom::calibration_library::light_cal as sl;\npub use sl::*;\n";
    assert_eq!(starloom_aliases(src), vec!["sl".to_string()]);
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use sl::*;".to_string()]);
    assert!(violation(&found[0], &[]).is_some());
}

#[test]
fn guard_flags_an_alias_excluded_name() {
    let src = "    use starloom::fits_writer::xisf_writer as lib;\n    pub use lib::write_xisf_f32_full;\n";
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use lib::write_xisf_f32_full;".to_string()]);
    assert!(violation(&found[0], &["write_xisf_f32_full"]).is_some());
}

#[test]
fn guard_passes_a_clean_multi_line_list() {
    let src = "// pub use starloom::x::*;\n\
               pub use starloom::stacking::measure::{\n\
               \x20   alpha, beta, // beta is fine\n\
               \x20   gamma,\n\
               };\n\
               pub use crate::models::Other;\n";
    let found = starloom_reexports(src);
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("gamma"));
    assert!(violation(&found[0], &["delta"]).is_none());
}

#[test]
fn guard_stops_at_the_first_semicolon() {
    let src = "pub use starloom::a::{b}; pub use other::*;\n";
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use starloom::a::{b};".to_string()]);
    assert!(violation(&found[0], &[]).is_none());
}
