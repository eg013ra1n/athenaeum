//! The shim guard (starloom phase 1, review focus 3): a core module that shadows a starloom item to add an
//! Athenaeum fact re-exports the rest of starloom's module by name. A glob would let a fact-less starloom entry point
//! (a writer without the creator, a measure without the gain, a flat-norm reader without the cards) resolve under an
//! old `athenaeum_core::` path, so each such module lists its names and never ends a `pub use starloom::…` in `::*`.

use std::path::PathBuf;

/// One file and the starloom names its `pub use starloom::…;` statements must never carry. `modules` are the starloom
/// modules the shim covers or that hold an excluded name: re-exporting one whole (`pub use starloom::…::<module>;`,
/// with or without `as`) would resolve every excluded name under the old path.
struct Shim {
    file: &'static str,
    modules: &'static [&'static str],
    excluded: &'static [&'static str],
}

const SHIMS: &[Shim] = &[
    Shim {
        file: "stacking/measure.rs",
        modules: &["measure"],
        excluded: &["measure_frame_full", "measure_frame_pass1_gain"],
    },
    Shim {
        file: "calibration_library/light_cal.rs",
        modules: &["light_cal"],
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
        modules: &["ln", "reference"],
        excluded: &["write_reference", "write_reference_saturation"],
    },
    Shim {
        file: "stacking/ln/reference.rs",
        modules: &["reference"],
        excluded: &[],
    },
    Shim {
        file: "fits_writer/mod.rs",
        modules: &["fits_writer", "xisf_writer"],
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

/// The statement prefixes that start a starloom path: with or without the leading `::` of the extern prelude.
fn starloom_prefixes(lead: &str) -> [String; 2] {
    [format!("{lead}starloom::"), format!("{lead}::starloom::")]
}

/// The names the file binds with `use starloom::… as <alias>;`, also inside braces
/// (`use starloom::{stacking::measure as m};`).
fn starloom_aliases(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in starloom_prefixes("use ") {
        for stmt in statements_starting_with(src, &prefix) {
            let body = stmt.trim_end_matches(';').trim_end();
            for part in body.split(" as ").skip(1) {
                let alias: String = part.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !alias.is_empty() && alias != "_" {
                    out.push(alias);
                }
            }
        }
    }
    out
}

/// Every statement of `src` that re-exports starloom: `pub use starloom::…;` (or `pub use ::starloom::…;`) and
/// `pub use <alias>::…;` for each alias the file binds to a starloom path.
fn starloom_reexports(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in starloom_prefixes("pub use ") {
        out.extend(statements_starting_with(src, &prefix));
    }
    for alias in starloom_aliases(src) {
        out.extend(statements_starting_with(src, &format!("pub use {alias}::")));
    }
    out
}

/// `tree` cut at its top-level commas.
fn split_top(tree: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut from) = (0usize, 0usize);
    for (at, c) in tree.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&tree[from..at]);
                from = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&tree[from..]);
    parts
}

/// The last path segment of every leaf of a use tree (`self` stands for its parent; an `as` rename is dropped).
fn leaf_names(tree: &str, parent: Option<&str>, out: &mut Vec<String>) {
    for item in split_top(tree) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let (Some(open), Some(close)) = (item.find('{'), item.rfind('}')) {
            let head = item[..open].trim().trim_end_matches("::");
            let last = head.rsplit("::").next().filter(|s| !s.is_empty());
            leaf_names(&item[open + 1..close], last, out);
            continue;
        }
        let path = item.split(" as ").next().unwrap_or(item).trim();
        let last = path.rsplit("::").next().unwrap_or(path).trim();
        if last == "self" {
            if let Some(parent) = parent {
                out.push(parent.to_string());
            }
        } else {
            out.push(last.to_string());
        }
    }
}

/// The last segments of the leaves a `pub use` statement re-exports.
fn reexported_leaves(stmt: &str) -> Vec<String> {
    let body = stmt.trim().trim_end_matches(';');
    let body = body.find("use ").map_or(body, |at| &body[at + 4..]);
    let mut out = Vec::new();
    leaf_names(body, None, &mut out);
    out
}

/// Why `stmt` may not stand in a shim that excludes `excluded` and covers `modules`, or `None` when it is clean: any
/// `*` is a glob (plain, braced, or after a comma), a whole covered module is never re-exported, and an excluded name
/// never appears.
fn violation(stmt: &str, modules: &[&str], excluded: &[&str]) -> Option<String> {
    if stmt.contains('*') {
        return Some("a module that shadows a starloom item re-exports it by name, never by glob".to_string());
    }
    if let Some(module) = reexported_leaves(stmt).iter().find_map(|leaf| modules.iter().find(|m| *m == leaf)) {
        return Some(format!(
            "`{module}` is a starloom module the shim covers: re-exported whole it resolves the excluded names; list \
             its items by name"
        ));
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
            if let Some(why) = violation(stmt, shim.modules, shim.excluded) {
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
    assert!(violation(&found[0], &[], &[]).is_some());
}

#[test]
fn guard_flags_braced_globs() {
    for src in ["pub use starloom::stacking::{measure::*};\n", "pub use starloom::stacking::measure::{self, *};\n"] {
        let found = starloom_reexports(src);
        assert_eq!(found.len(), 1, "{src}");
        assert!(violation(&found[0], &[], &[]).is_some(), "{src}");
    }
}

#[test]
fn guard_flags_an_alias_glob() {
    let src = "use starloom::calibration_library::light_cal as sl;\npub use sl::*;\n";
    assert_eq!(starloom_aliases(src), vec!["sl".to_string()]);
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use sl::*;".to_string()]);
    assert!(violation(&found[0], &[], &[]).is_some());
}

#[test]
fn guard_flags_an_alias_excluded_name() {
    let src = "    use starloom::fits_writer::xisf_writer as lib;\n    pub use lib::write_xisf_f32_full;\n";
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use lib::write_xisf_f32_full;".to_string()]);
    assert!(violation(&found[0], &[], &["write_xisf_f32_full"]).is_some());
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
    assert!(violation(&found[0], &[], &["delta"]).is_none());
}

#[test]
fn guard_stops_at_the_first_semicolon() {
    let src = "pub use starloom::a::{b}; pub use other::*;\n";
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use starloom::a::{b};".to_string()]);
    assert!(violation(&found[0], &[], &[]).is_none());
}

#[test]
fn guard_flags_a_whole_module_reexport() {
    for src in [
        "pub use starloom::stacking::measure as kernel;\n",
        "pub use starloom::stacking::ln::reference;\n",
        "pub use starloom::stacking::{measure};\n",
        "pub use starloom::stacking::ln::{self as l};\n",
        "pub use starloom::stacking::{ln::{self}, other};\n",
    ] {
        let found = starloom_reexports(src);
        assert_eq!(found.len(), 1, "{src}");
        assert!(violation(&found[0], &["measure", "ln", "reference"], &[]).is_some(), "{src}");
    }
    let found = starloom_reexports("pub use starloom::stacking::measure::{alpha, reference_stars};\n");
    assert!(violation(&found[0], &["measure", "reference"], &[]).is_none());
}

#[test]
fn guard_accepts_a_leading_double_colon() {
    let src = "pub use ::starloom::stacking::measure::*;\npub use ::starloom::a::b;\n";
    let found = starloom_reexports(src);
    assert_eq!(found.len(), 2);
    assert!(violation(&found[0], &[], &[]).is_some());
    assert!(violation(&found[1], &["b"], &[]).is_some());
    let src = "use ::starloom::stacking::measure as m;\npub use m::*;\n";
    assert_eq!(starloom_aliases(src), vec!["m".to_string()]);
    assert_eq!(starloom_reexports(src), vec!["pub use m::*;".to_string()]);
}

#[test]
fn guard_reads_aliases_bound_inside_braces() {
    let src = "use starloom::{stacking::measure as m};\npub use m::*;\n";
    assert_eq!(starloom_aliases(src), vec!["m".to_string()]);
    let found = starloom_reexports(src);
    assert_eq!(found, vec!["pub use m::*;".to_string()]);
    assert!(violation(&found[0], &[], &[]).is_some());
    let src = "use starloom::{a as x, b::{c as y}, d};\n";
    assert_eq!(starloom_aliases(src), vec!["x".to_string(), "y".to_string()]);
}
