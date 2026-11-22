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
        excluded: &["write_xisf_f32_full", "write_xisf_f32_to_full"],
    },
];

/// Every `pub use starloom::…;` statement of `src`, lines joined up to the `;`.
fn starloom_reexports(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if t.starts_with("//") || !t.starts_with("pub use starloom::") {
            continue;
        }
        let mut stmt = t.to_string();
        while !stmt.contains(';') {
            match lines.next() {
                Some(next) => {
                    stmt.push(' ');
                    stmt.push_str(next.trim());
                }
                None => break,
            }
        }
        out.push(stmt);
    }
    out
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
            assert!(
                !stmt.trim_end_matches(';').trim_end().ends_with("::*"),
                "{}: a module that shadows a starloom item re-exports it by name (starloom phase 1): `{stmt}`",
                shim.file
            );
            for word in shim.excluded {
                assert!(
                    !has_word(stmt, word),
                    "{}: `{word}` is a starloom form without the Athenaeum fact and must not be re-exported: `{stmt}`",
                    shim.file
                );
            }
        }
    }
}
