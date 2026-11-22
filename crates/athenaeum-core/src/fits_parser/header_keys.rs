//! One key reader per stored header format.
//!
//! Every place that turns a header into catalog fields (the FITS and XISF frame builders, the
//! metadata pane's snapshot, the override recomputation, Apply) reads a [`HeaderKeyMap`] and
//! hands it to the one resolver in `crate::header_fallbacks`. The map is built by the scanner's
//! own reading of each format, so a stored header answers exactly as the file did when it was
//! scanned:
//!
//! - FITS: the scanner's card state machine (`FitsHeader::from_header_text`), HIERARCH, doubled
//!   quotes and CONTINUE cards included.
//! - XISF: the scanner's `FITSKeyword` extraction (`xisf_keyword_pairs`), or the `NAME = value`
//!   dump Athenaeum stores (`xisf_header_text`), which this module inverts.
//!
//! Keys are normalised with [`normalize_keyword`]; values are the raw text of the format (the
//! resolver trims).

use super::fits_header_reader::FitsHeader;
use crate::models::FileFormat;
use std::collections::HashMap;
use std::path::Path;

/// First line of the XISF keyword dump `xisf_header_text` writes: title, rule and a blank line.
pub(crate) const XISF_DUMP_BANNER: &str = "XISF FITS Keywords:\n==================\n\n";

/// A header keyword as the rules and the readers compare it: trimmed and upper-cased, so a user
/// keyword matches a HIERARCH name whatever its case (inner spaces kept). `None` when the result
/// is empty or contains `=` (such a name cannot be a keyword).
pub fn normalize_keyword(raw: &str) -> Option<String> {
    let k = raw.trim().to_uppercase();
    if k.is_empty() || k.contains('=') {
        None
    } else {
        Some(k)
    }
}

/// Normalised keyword -> raw value of one stored header.
#[derive(Debug, Clone, Default)]
pub struct HeaderKeyMap {
    map: HashMap<String, String>,
}

impl HeaderKeyMap {
    /// The raw value of a keyword (the key is normalised here, so `"ccd-temp"` finds `CCD-TEMP`).
    pub fn get(&self, key: &str) -> Option<&str> {
        match self.map.get(key) {
            Some(v) => Some(v.as_str()),
            None => normalize_keyword(key).and_then(|k| self.map.get(&k)).map(String::as_str),
        }
    }

    /// True when the header holds any keyword other than `COMMENT` / `HISTORY`.
    pub fn has_keywords(&self) -> bool {
        self.map.keys().any(|k| k != "COMMENT" && k != "HISTORY")
    }

    /// The map the older readers returned: UPPERCASE keys, no `COMMENT` / `HISTORY`, values
    /// trimmed and blank ones dropped.
    pub fn into_legacy_map(self) -> HashMap<String, String> {
        self.map
            .into_iter()
            .filter(|(k, _)| k != "COMMENT" && k != "HISTORY")
            .filter_map(|(k, v)| {
                let t = v.trim();
                if t.is_empty() {
                    None
                } else {
                    Some((k, t.to_string()))
                }
            })
            .collect()
    }

    /// Card order, first occurrence wins (what a scan does).
    pub(crate) fn from_fits_header(h: &FitsHeader) -> Self {
        Self::from_first_wins(h.keywords_in_order().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    /// Later pairs replace earlier ones (XISF `FITSKeyword` elements and dump lines).
    pub(crate) fn from_last_wins(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            if let Some(k) = normalize_keyword(&k) {
                map.insert(k, v);
            }
        }
        Self { map }
    }

    /// Earlier pairs win.
    pub(crate) fn from_first_wins(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            if let Some(k) = normalize_keyword(&k) {
                map.entry(k).or_insert(v);
            }
        }
        Self { map }
    }
}

/// The key map of a stored header blob (`fits_header.header`) of the given format.
pub fn stored_header_key_map(format: FileFormat, text: &str) -> HeaderKeyMap {
    match format {
        FileFormat::FITS => {
            let keys = HeaderKeyMap::from_fits_header(&FitsHeader::from_header_text(text));
            if keys.has_keywords() {
                keys
            } else {
                // Hand-written `KEY = value` text, never a scanner output.
                HeaderKeyMap::from_first_wins(legacy_eq_text_pairs(text))
            }
        }
        FileFormat::XISF => {
            if let Some(rest) = text.strip_prefix(XISF_DUMP_BANNER) {
                return HeaderKeyMap::from_last_wins(xisf_dump_pairs(rest));
            }
            if text.contains("<?xml") {
                if let Ok(pairs) = super::xisf_keyword_pairs(text.as_bytes(), Path::new("")) {
                    let keys = HeaderKeyMap::from_last_wins(pairs);
                    if keys.has_keywords() {
                        return keys;
                    }
                }
            }
            HeaderKeyMap::from_first_wins(legacy_eq_text_pairs(text))
        }
    }
}

/// Inverse of `xisf_header_text`'s body: one `NAME = value` per line, split at the first
/// `" = "`; a line ending in `" ="` has an empty value. Values are verbatim (no comment split).
fn xisf_dump_pairs(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in body.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(" = ") {
            out.push((name.to_string(), value.to_string()));
        } else if let Some(name) = line.strip_suffix(" =") {
            out.push((name.to_string(), String::new()));
        }
    }
    out
}

/// A plain `KEY = value` header dump (one keyword per line; banner, separator, COMMENT, HISTORY
/// and blank lines skipped). Quoted strings lose their quotes; an unquoted value's trailing
/// `" / comment"` is dropped. Hand-written text only: no scanner output takes this path.
fn legacy_eq_text_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().to_uppercase();
        if key.is_empty()
            || key.contains(' ')
            || key == "END"
            || key == "COMMENT"
            || key == "HISTORY"
        {
            continue;
        }
        let v = v.trim();
        let value = if let Some(s) = v.strip_prefix('\'') {
            match s.find('\'') {
                Some(end) => s[..end].trim_end().to_string(),
                None => continue,
            }
        } else {
            // Drop a FITS-style trailing comment (" / ...") without breaking
            // space-separated values like "2 41 04.824".
            v.split(" / ").next().unwrap_or("").trim().to_string()
        };
        if value.is_empty() {
            continue;
        }
        out.push((key, value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keyword_trims_uppercases_and_refuses_empty_or_equals() {
        assert_eq!(normalize_keyword(" det-temp "), Some("DET-TEMP".into()));
        assert_eq!(normalize_keyword("eso det temp"), Some("ESO DET TEMP".into()));
        assert_eq!(normalize_keyword("   "), None);
        assert_eq!(normalize_keyword("A=B"), None);
    }

    #[test]
    fn xisf_dump_reader_inverts_xisf_header_text() {
        let dump = format!("{XISF_DUMP_BANNER}FILTER = Ha / 7nm\nGAIN = 100\nGAIN = 120\nOBJECT = \n");
        let m = stored_header_key_map(FileFormat::XISF, &dump);
        assert_eq!(m.get("FILTER"), Some("Ha / 7nm"));
        assert_eq!(m.get("GAIN"), Some("120"));
        assert_eq!(m.get("OBJECT"), Some(""));
    }

    #[test]
    fn xisf_dump_reader_reads_a_trimmed_empty_value() {
        let dump = format!("{XISF_DUMP_BANNER}OBJECT =\nGAIN = 5");
        let m = stored_header_key_map(FileFormat::XISF, &dump);
        assert_eq!(m.get("OBJECT"), Some(""));
        assert_eq!(m.get("GAIN"), Some("5"));
    }

    #[test]
    fn xisf_placeholder_has_no_keywords() {
        let p = "Header extracted from: /a=b/x.xisf\n(No FITS keywords found in XISF)";
        assert!(!stored_header_key_map(FileFormat::XISF, p).has_keywords());
    }

    #[test]
    fn fits_hand_dump_falls_back_to_eq_text() {
        assert_eq!(
            stored_header_key_map(FileFormat::FITS, "SIMPLE = T\nFOCALLEN = 140.6").get("FOCALLEN"),
            Some("140.6")
        );
    }

    #[test]
    fn hierarch_keys_match_case_insensitively() {
        let card = format!("{:<80}", "HIERARCH Eso Det Temp = -7.25 / sensor");
        let m = stored_header_key_map(FileFormat::FITS, &card);
        assert_eq!(m.get("ESO DET TEMP"), Some("-7.25"));
        assert_eq!(m.get("eso det temp"), Some("-7.25"));
    }

    #[test]
    fn xisf_raw_xml_reads_the_scanners_extraction() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf>
  <FITSKeyword name="Filter" value="'Ha / 7nm'" comment="" />
  <FITSKeyword name="GAIN" value="100" />
  <FITSKeyword name="GAIN" value="120" />
</xisf>"#;
        let m = stored_header_key_map(FileFormat::XISF, xml);
        assert_eq!(m.get("FILTER"), Some("Ha / 7nm"));
        assert_eq!(m.get("GAIN"), Some("120"));
    }

    #[test]
    fn legacy_map_drops_comment_history_and_trims() {
        let cards = [
            format!("{:<80}", "COMMENT some text"),
            format!("{:<80}", "HISTORY something"),
            format!("{:<80}", "OBJECT  = ''"),
            format!("{:<80}", "FILTER  = '  Ha  '"),
            format!("{:<80}", "END"),
        ]
        .join("\n");
        let legacy = stored_header_key_map(FileFormat::FITS, &cards).into_legacy_map();
        assert!(!legacy.contains_key("COMMENT"));
        assert!(!legacy.contains_key("HISTORY"));
        assert!(!legacy.contains_key("OBJECT"));
        assert_eq!(legacy.get("FILTER").map(String::as_str), Some("Ha"));
    }
}
