//! Automatic filter-name matching against a project's cached dictionary
//! (spec 2026-09-23 collab v3, plan ruling P3).
//!
//! Wave 2 has no normalizer UI and no `filter_mappings` table: a frame's
//! trimmed `FILTER` header is matched case-insensitively against every
//! dictionary entry's `canonical` spelling and its `aliases`. No match fails
//! the collab gate with the raw name quoted (`collab::gate::evaluate_frame`),
//! so the operator can see exactly what didn't map. The normaliser and the
//! mapping modal are wave 3's.

use serde::{Deserialize, Serialize};

/// One dictionary entry as the hub's `GET /projects/{id}/dictionary` response
/// carries it, and as `db::collab::CollabProjectRow::dictionary_json` caches
/// the array of them (filled by the version-poll, Task 8).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
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
}
