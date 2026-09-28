//! Filter-name resolution against a project's cached dictionary and the
//! account's explicit mappings (spec 2026-09-28 §3.2–§3.3, plan ruling P3).
//!
//! `resolve_filter` is the full order: an explicit mapping row first, then
//! wave 2's automatic dictionary match (`match_filter`, unchanged, now step 3
//! of the order) as the fallback, else unmapped. `propose_canonical` is the
//! pure normaliser the mapping modal preselects with — it never resolves a
//! frame and never writes anything (F3).

use crate::db::collab::FilterMappingRow;
use serde::{Deserialize, Serialize};

/// One dictionary entry as the hub's `GET /projects/{id}/dictionary` response
/// carries it, and as `db::collab::CollabProjectRow::dictionary_json` caches
/// the array of them (filled by the version-poll, Task 8).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub kind: String,
}

/// Match a filter name against the project's dictionary: trim `raw`, compare
/// case-insensitively against every entry's `canonical` and each of its
/// `aliases`, and return that entry's `canonical` spelling (never the raw
/// one, and never re-cased) on a hit — the hub's `filterCanonical` rule is
/// "exact and case-sensitive", so only the dictionary's own spelling may ever
/// be sent. `None` for an empty/whitespace-only `raw` or no match anywhere in
/// `dict`.
pub fn match_filter(raw: &str, dict: &[DictionaryEntry]) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let needle = trimmed.to_lowercase();
    for entry in dict {
        if entry.canonical.to_lowercase() == needle {
            return Some(entry.canonical.clone());
        }
        if entry.aliases.iter().any(|a| a.to_lowercase() == needle) {
            return Some(entry.canonical.clone());
        }
    }
    None
}

/// Spec 2026-09-28 §3.2 — how one frame's raw `FILTER` resolves for one
/// project. Only `Mapped`/`Matched` may reach `ATH_FILT` and the announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterResolution {
    /// A mapping row of the account, and the project's dictionary has it.
    Mapped(String),
    /// A mapping row exists, but this project's dictionary lacks the canonical.
    MappedToMissing(String),
    /// No row; the raw name equals a canonical or alias (P3, `match_filter`).
    Matched(String),
    /// None of the above; an empty raw name without a row is always here (F1).
    Unmapped,
}

impl FilterResolution {
    pub fn canonical(&self) -> Option<&str> {
        match self {
            FilterResolution::Mapped(c) | FilterResolution::Matched(c) => Some(c),
            FilterResolution::MappedToMissing(_) | FilterResolution::Unmapped => None,
        }
    }
    pub fn is_unresolved(&self) -> bool {
        matches!(self, FilterResolution::MappedToMissing(_) | FilterResolution::Unmapped)
    }
}

/// Explicit mapping → dictionary exact/alias → unmapped. `raw` and
/// `instrume` are trimmed here; `mappings` are the account's rows.
pub fn resolve_filter(
    raw: &str,
    instrume: &str,
    mappings: &[FilterMappingRow],
    dict: &[DictionaryEntry],
) -> FilterResolution {
    let raw = raw.trim();
    let instrume = instrume.trim();
    if let Some(m) = mappings.iter().find(|m| m.instrume == instrume && m.filter_raw == raw) {
        return if dict.iter().any(|e| e.canonical == m.canonical) {
            FilterResolution::Mapped(m.canonical.clone())
        } else {
            FilterResolution::MappedToMissing(m.canonical.clone())
        };
    }
    match match_filter(raw, dict) {
        Some(c) => FilterResolution::Matched(c),
        None => FilterResolution::Unmapped,
    }
}

const VENDOR_TOKENS: [&str; 8] = ["filter", "astronomik", "baader", "optolong", "antlia", "chroma", "zwo", "svbony"];

/// The fixed synonym table of spec §3.3 step 4: normalised key → canonical
/// NAME (matched against the dictionary case-insensitively, its spelling
/// returned). Single letters h/o/s come from the dev catalog's 4 228 frames.
const SYNONYMS: [(&str, &str); 26] = [
    ("l", "L"), ("lum", "L"), ("luminance", "L"), ("clear", "L"),
    ("r", "R"), ("red", "R"),
    ("g", "G"), ("green", "G"),
    ("b", "B"), ("blue", "B"),
    ("ha", "Ha"), ("h", "Ha"), ("h-alpha", "Ha"), ("halpha", "Ha"), ("h_alpha", "Ha"), ("hα", "Ha"), ("h-a", "Ha"),
    ("oiii", "OIII"), ("o3", "OIII"), ("o", "OIII"), ("o-iii", "OIII"), ("o_iii", "OIII"),
    ("sii", "SII"), ("s2", "SII"), ("s", "SII"), ("s-ii", "SII"),
];

/// Spec §3.3: lower-case, trim, collapse spaces; drop a trailing `<n>nm`
/// token; drop vendor tokens; look up the synonym table (target must be a
/// canonical of `dict`), else `match_filter` on the remainder, else none.
/// An empty raw proposes the dictionary's sole `unfiltered` entry. Never
/// writes anything — the modal preselects with it (F3).
pub fn propose_canonical(raw: &str, dict: &[DictionaryEntry]) -> Option<String> {
    let mut key = raw.trim().to_lowercase();
    if key.is_empty() {
        let mut unfiltered = dict.iter().filter(|e| e.kind == "unfiltered");
        return match (unfiltered.next(), unfiltered.next()) {
            (Some(only), None) => Some(only.canonical.clone()),
            _ => None,
        };
    }
    key = key.split_whitespace().collect::<Vec<_>>().join(" ");
    // Trailing bandwidth: "ha 3nm", "oiii 6.5nm", "ha3nm".
    if let Some(idx) = key.rfind("nm") {
        if idx + 2 == key.len() {
            let head = key[..idx].trim_end();
            let digits_start = head.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').len();
            if digits_start < head.len() {
                key = head[..digits_start].trim_end().to_string();
            }
        }
    }
    let tokens: Vec<&str> = key
        .split(|c: char| c == ' ' || c == '-' || c == '_')
        .filter(|t| !t.is_empty() && !VENDOR_TOKENS.contains(t))
        .collect();
    let joined_space = tokens.join(" ");
    let joined_dash = tokens.join("-");
    let joined_under = tokens.join("_");
    let joined_none = tokens.concat();
    let lookup = |k: &str| {
        SYNONYMS
            .iter()
            .find(|(s, _)| *s == k)
            .and_then(|(_, name)| dict.iter().find(|e| e.canonical.eq_ignore_ascii_case(name)))
            .map(|e| e.canonical.clone())
    };
    let resolved = [joined_space.as_str(), joined_dash.as_str(), joined_under.as_str(), joined_none.as_str()]
        .into_iter()
        .find_map(lookup)
        .or_else(|| match_filter(&joined_space, dict))
        .or_else(|| match_filter(&joined_none, dict));
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_canonical_and_aliases_case_insensitively() {
        let d = vec![
            DictionaryEntry {
                canonical: "R".into(),
                aliases: vec!["Red".into()],
                kind: "broadband".into(),
            },
            DictionaryEntry {
                canonical: "Ha".into(),
                aliases: vec!["H-alpha".into(), "Halpha".into()],
                kind: "narrowband".into(),
            },
        ];
        assert_eq!(match_filter(" red ", &d).as_deref(), Some("R"));
        assert_eq!(match_filter("r", &d).as_deref(), Some("R"));
        assert_eq!(match_filter("H-ALPHA", &d).as_deref(), Some("Ha"));
        assert_eq!(match_filter("OIII", &d), None);
        assert_eq!(match_filter("", &d), None);
    }

    #[test]
    fn dictionary_entries_deserialize_camel_case_with_default_aliases() {
        let d: Vec<DictionaryEntry> =
            serde_json::from_str(r#"[{"canonical":"L","kind":"broadband"}]"#).unwrap();
        assert_eq!(d[0].canonical, "L");
        assert!(d[0].aliases.is_empty());
        assert_eq!(match_filter("l", &d).as_deref(), Some("L"));
    }

    fn dict() -> Vec<DictionaryEntry> {
        serde_json::from_str(
            r#"[{"canonical":"L","aliases":["lum","luminance","clear","l"],"kind":"luminance"},
                {"canonical":"R","aliases":["red","r"],"kind":"broadband"},
                {"canonical":"Ha","aliases":["h-alpha","halpha","h_alpha","hα","ha"],"kind":"narrowband"},
                {"canonical":"OIII","aliases":["o3","oiii","o-iii"],"kind":"narrowband"},
                {"canonical":"SII","aliases":["s2","sii","s-ii"],"kind":"narrowband"},
                {"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}]"#,
        )
        .unwrap()
    }
    fn map(instrume: &str, raw: &str, canonical: &str) -> crate::db::collab::FilterMappingRow {
        crate::db::collab::FilterMappingRow { account: "a@x.io".into(), instrume: instrume.into(), filter_raw: raw.into(), canonical: canonical.into() }
    }

    #[test]
    fn resolution_order_mapping_then_alias_then_unmapped() {
        let d = dict();
        let m = vec![map("QHY268M", "Slot 0", "Ha"), map("QHY268M", "", "None"), map("QHY268M", "L", "None"), map("ASI294", "H", "Hb")];
        assert_eq!(resolve_filter("Slot 0", "QHY268M", &m, &d), FilterResolution::Mapped("Ha".into()));
        assert_eq!(resolve_filter("", "QHY268M", &m, &d), FilterResolution::Mapped("None".into()));
        // Explicit mapping beats the alias hit.
        assert_eq!(resolve_filter("L", "QHY268M", &m, &d), FilterResolution::Mapped("None".into()));
        // Alias hit without a mapping row, dictionary spelling returned.
        assert_eq!(resolve_filter(" red ", "QHY268M", &m, &d), FilterResolution::Matched("R".into()));
        // Mapping whose canonical the dictionary lacks.
        assert_eq!(resolve_filter("H", "ASI294", &m, &d), FilterResolution::MappedToMissing("Hb".into()));
        // Another camera: the QHY mapping does not apply.
        assert_eq!(resolve_filter("Slot 0", "ASI294", &m, &d), FilterResolution::Unmapped);
        // Empty raw without a row is always Unmapped (F1), never alias-matched.
        assert_eq!(resolve_filter("", "ASI294", &m, &d), FilterResolution::Unmapped);
        assert_eq!(resolve_filter("Slot 0", "QHY268M", &m, &d).canonical(), Some("Ha"));
        assert!(resolve_filter("H", "ASI294", &m, &d).is_unresolved());
        assert!(!resolve_filter(" red ", "QHY268M", &m, &d).is_unresolved());
    }

    #[test]
    fn normaliser_proposes_only_what_the_dictionary_offers() {
        let d = dict();
        for (raw, want) in [
            ("Red", Some("R")), ("LUM", Some("L")), ("Clear", Some("L")),
            ("H", Some("Ha")), ("h-alpha", Some("Ha")), ("Ha 3nm", Some("Ha")), ("Baader Ha 3.5nm", Some("Ha")),
            ("O", Some("OIII")), ("O3", Some("OIII")), ("S", Some("SII")), ("s2", Some("SII")),
            ("Astronomik OIII-filter", Some("OIII")),
            ("Slot 0", None), ("Filter#1", None), ("1", None), ("UV/IR cut", None), ("Dualband", None),
        ] {
            assert_eq!(propose_canonical(raw, &d).as_deref(), want, "{raw:?}");
        }
        // Empty raw: the sole unfiltered entry.
        assert_eq!(propose_canonical("", &d).as_deref(), Some("None"));
        let mut two = d.clone();
        two.push(DictionaryEntry { canonical: "Open".into(), aliases: vec![], kind: "unfiltered".into() });
        assert_eq!(propose_canonical("", &two), None, "two unfiltered entries: no proposal");
        // A synonym whose target the dictionary lacks: alias fallback, then none.
        let no_ha: Vec<DictionaryEntry> = d.iter().filter(|e| e.canonical != "Ha").cloned().collect();
        assert_eq!(propose_canonical("H", &no_ha), None);
        // Green is a synonym but this dictionary has no G.
        assert_eq!(propose_canonical("Green", &d), None);
    }
}
