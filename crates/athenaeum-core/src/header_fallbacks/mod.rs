//! User-ordered header keywords for the twelve catalog fields read from a FITS or XISF header
//! (issue #21).
//!
//! `HeaderFallbackConfig` says, per field, which keywords are tried and in what order. It is an
//! explicit argument of every function that turns a header into a `Frame`; the one resolver in
//! [`resolve`] applies it to a `HeaderKeyMap` (see `fits_parser::header_keys`).

pub mod resolve;

pub use resolve::{
    resolve_all, resolve_field, resolve_keyword, same_value, FieldValue, ResolvedFields,
};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Number or text: how a keyword's raw value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Number,
    Text,
}

/// The twelve catalog fields whose header keywords the user orders. Declaration order is the
/// order of the spec's table (and of the Settings section).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS,
)]
#[serde(rename_all = "camelCase")]
pub enum HeaderField {
    CcdTemp,
    SetTemp,
    Gain,
    Offset,
    Exptime,
    Focallen,
    Xpixsz,
    Ypixsz,
    Object,
    Filter,
    Instrume,
    Telescop,
}

impl HeaderField {
    pub const ALL: [HeaderField; 12] = [
        HeaderField::CcdTemp,
        HeaderField::SetTemp,
        HeaderField::Gain,
        HeaderField::Offset,
        HeaderField::Exptime,
        HeaderField::Focallen,
        HeaderField::Xpixsz,
        HeaderField::Ypixsz,
        HeaderField::Object,
        HeaderField::Filter,
        HeaderField::Instrume,
        HeaderField::Telescop,
    ];

    /// The id used on the wire and in the settings document (camelCase).
    pub fn id(self) -> &'static str {
        match self {
            HeaderField::CcdTemp => "ccdTemp",
            HeaderField::SetTemp => "setTemp",
            HeaderField::Gain => "gain",
            HeaderField::Offset => "offset",
            HeaderField::Exptime => "exptime",
            HeaderField::Focallen => "focallen",
            HeaderField::Xpixsz => "xpixsz",
            HeaderField::Ypixsz => "ypixsz",
            HeaderField::Object => "object",
            HeaderField::Filter => "filter",
            HeaderField::Instrume => "instrume",
            HeaderField::Telescop => "telescop",
        }
    }

    /// The `frames` column the field is stored in.
    pub fn column(self) -> &'static str {
        match self {
            HeaderField::CcdTemp => "ccd_temp",
            HeaderField::SetTemp => "set_temp",
            HeaderField::Gain => "gain",
            HeaderField::Offset => "offset",
            HeaderField::Exptime => "exptime",
            HeaderField::Focallen => "focallen",
            HeaderField::Xpixsz => "xpixsz",
            HeaderField::Ypixsz => "ypixsz",
            HeaderField::Object => "object",
            HeaderField::Filter => "filter",
            HeaderField::Instrume => "instrume",
            HeaderField::Telescop => "telescop",
        }
    }

    pub fn kind(self) -> FieldKind {
        match self {
            HeaderField::Object
            | HeaderField::Filter
            | HeaderField::Instrume
            | HeaderField::Telescop => FieldKind::Text,
            _ => FieldKind::Number,
        }
    }

    /// The keywords tried when the user has not changed the field, in order.
    pub fn default_chain(self) -> &'static [&'static str] {
        match self {
            HeaderField::CcdTemp => &["CCD-TEMP", "DET-TEMP"],
            HeaderField::SetTemp => &["SET-TEMP"],
            HeaderField::Gain => &["GAIN"],
            HeaderField::Offset => &["OFFSET"],
            HeaderField::Exptime => &["EXPTIME", "EXPOSURE"],
            HeaderField::Focallen => &["FOCALLEN"],
            HeaderField::Xpixsz => &["XPIXSZ", "PIXSIZE1"],
            HeaderField::Ypixsz => &["YPIXSZ", "PIXSIZE2"],
            HeaderField::Object => &["OBJECT"],
            HeaderField::Filter => &["FILTER"],
            HeaderField::Instrume => &["INSTRUME"],
            HeaderField::Telescop => &["TELESCOP"],
        }
    }
}

/// One field's keyword order, and the keywords the user removed from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FieldRule {
    /// Keywords tried first to last; the first usable value wins.
    pub order: Vec<String>,
    /// Keywords the user took out of the order, newest last. Never read by a scan.
    #[serde(default)]
    pub retired: Vec<String>,
}

/// The rules of all twelve fields. A field absent from `fields` uses its default chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HeaderFallbackConfig {
    #[serde(default)]
    pub fields: BTreeMap<HeaderField, FieldRule>,
}

impl Default for HeaderFallbackConfig {
    /// The full document: every field, its default chain, nothing retired.
    fn default() -> Self {
        let fields = HeaderField::ALL
            .iter()
            .map(|&f| {
                (
                    f,
                    FieldRule {
                        order: f.default_chain().iter().map(|k| (*k).to_string()).collect(),
                        retired: Vec::new(),
                    },
                )
            })
            .collect();
        Self { fields }
    }
}

static DEFAULT: LazyLock<HeaderFallbackConfig> = LazyLock::new(HeaderFallbackConfig::default);

impl HeaderFallbackConfig {
    /// The keyword order of a field; a field absent from the document reads its default chain.
    pub fn order(&self, f: HeaderField) -> &[String] {
        self.fields
            .get(&f)
            .or_else(|| DEFAULT.fields.get(&f))
            .map(|r| r.order.as_slice())
            .unwrap_or(&[])
    }

    /// The keywords the user removed from a field's order (empty when none).
    pub fn retired(&self, f: HeaderField) -> &[String] {
        self.fields.get(&f).map(|r| r.retired.as_slice()).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_all_twelve_fields_with_their_chains() {
        let d = HeaderFallbackConfig::default();
        assert_eq!(d.fields.len(), 12);
        assert_eq!(d.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP"]);
        assert_eq!(d.order(HeaderField::Exptime), ["EXPTIME", "EXPOSURE"]);
        assert_eq!(d.order(HeaderField::Ypixsz), ["YPIXSZ", "PIXSIZE2"]);
        assert!(d.retired(HeaderField::Gain).is_empty());
    }

    #[test]
    fn an_absent_field_reads_its_default_chain() {
        let empty = HeaderFallbackConfig { fields: BTreeMap::new() };
        for f in HeaderField::ALL {
            assert_eq!(empty.order(f), f.default_chain(), "{}", f.id());
        }
    }

    #[test]
    fn ids_match_the_serde_names_and_kinds_split_eight_four() {
        for f in HeaderField::ALL {
            let json = serde_json::to_string(&f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.id()));
        }
        let numbers = HeaderField::ALL.iter().filter(|f| f.kind() == FieldKind::Number).count();
        assert_eq!(numbers, 8);
    }

    #[test]
    fn config_round_trips_through_json() {
        let d = HeaderFallbackConfig::default();
        let back: HeaderFallbackConfig = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }
}
