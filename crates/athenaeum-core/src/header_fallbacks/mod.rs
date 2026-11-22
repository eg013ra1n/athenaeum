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

use crate::fits_parser::header_keys::normalize_keyword;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
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

/// Why a rules document is refused. `label` in the messages is the field id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesError {
    /// A keyword that is empty or contains `=`.
    InvalidKeyword { field: HeaderField, keyword: String },
    /// A keyword listed twice (after normalisation).
    Duplicate { field: HeaderField, keyword: String },
    /// A default keyword of the field that the order lacks.
    MissingDefault { field: HeaderField, keyword: String },
}

impl std::fmt::Display for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RulesError::InvalidKeyword { field, keyword } => write!(
                f,
                "{}: \"{}\" is not a valid keyword (empty or contains '=')",
                field.id(),
                keyword
            ),
            RulesError::Duplicate { field, keyword } => {
                write!(f, "{}: {} appears twice", field.id(), keyword)
            }
            RulesError::MissingDefault { field, keyword } => write!(
                f,
                "{}: the standard keyword {} cannot be removed",
                field.id(),
                keyword
            ),
        }
    }
}

impl std::error::Error for RulesError {}

/// Keep the last occurrence of each keyword, in order.
fn dedupe_keep_last(list: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = list.into_iter().rev().filter(|k| seen.insert(k.clone())).collect();
    out.reverse();
    out
}

/// The expanded document, every keyword normalised, or the first rule that is broken. A field
/// absent from `cfg` reads its default chain. Repeats are checked after normalisation, every
/// default keyword of a field must be present, and `retired` is deduplicated (last occurrence
/// kept) and stripped of the field's `order` keywords. A `retired` entry that is not a valid
/// keyword is dropped (nothing reads `retired`).
pub fn validate(cfg: &HeaderFallbackConfig) -> Result<HeaderFallbackConfig, RulesError> {
    let mut fields = BTreeMap::new();
    for field in HeaderField::ALL {
        let order_in = cfg.order(field);
        let mut order: Vec<String> = Vec::with_capacity(order_in.len());
        for raw in order_in {
            let Some(k) = normalize_keyword(raw) else {
                return Err(RulesError::InvalidKeyword { field, keyword: raw.clone() });
            };
            if order.contains(&k) {
                return Err(RulesError::Duplicate { field, keyword: k });
            }
            order.push(k);
        }
        for d in field.default_chain() {
            if !order.iter().any(|k| k == d) {
                return Err(RulesError::MissingDefault { field, keyword: (*d).to_string() });
            }
        }
        let retired = dedupe_keep_last(
            cfg.retired(field).iter().filter_map(|r| normalize_keyword(r)).collect(),
        )
        .into_iter()
        .filter(|k| !order.contains(k))
        .collect();
        fields.insert(field, FieldRule { order, retired });
    }
    Ok(HeaderFallbackConfig { fields })
}

/// A stored order that lacks a default keyword gets it back (D14, the import rule of the spec's
/// §5.7): after its predecessor in the default chain, first when it has none. Entries are compared
/// normalised; an entry that does not normalise is kept as written (`validate` refuses it).
pub(crate) fn fill_missing_defaults(field: HeaderField, order: Vec<String>) -> Vec<String> {
    let mut order = order;
    let chain = field.default_chain();
    for (i, d) in chain.iter().enumerate() {
        let position_of = |order: &[String], key: &str| {
            order.iter().position(|k| normalize_keyword(k).as_deref() == Some(key))
        };
        if position_of(&order, d).is_some() {
            continue;
        }
        let at = i
            .checked_sub(1)
            .and_then(|p| position_of(&order, chain[p]))
            .map_or(0, |p| p + 1);
        order.insert(at, (*d).to_string());
    }
    order
}

impl HeaderFallbackConfig {
    /// The compact stored form: only the fields whose order differs from the default chain or
    /// whose `retired` is not empty.
    pub fn compact(&self) -> HeaderFallbackConfig {
        let fields = HeaderField::ALL
            .iter()
            .filter_map(|&f| {
                let order = self.order(f);
                let retired = self.retired(f);
                let default_order = order.len() == f.default_chain().len()
                    && order.iter().zip(f.default_chain()).all(|(a, b)| a == *b);
                if default_order && retired.is_empty() {
                    None
                } else {
                    Some((f, FieldRule { order: order.to_vec(), retired: retired.to_vec() }))
                }
            })
            .collect();
        HeaderFallbackConfig { fields }
    }

    /// D12: the server keeps `retired`. Per field,
    /// `retired = (previous.retired ++ self.retired ++ (previous.order - self.order)) - self.order`,
    /// newest last. Returns the expanded document.
    pub fn with_retirements_from(self, previous: &HeaderFallbackConfig) -> HeaderFallbackConfig {
        let fields = HeaderField::ALL
            .iter()
            .map(|&f| {
                let order = self.order(f).to_vec();
                let mut retired: Vec<String> = previous.retired(f).to_vec();
                retired.extend(self.retired(f).iter().cloned());
                retired.extend(previous.order(f).iter().filter(|k| !order.contains(k)).cloned());
                let retired =
                    dedupe_keep_last(retired).into_iter().filter(|k| !order.contains(k)).collect();
                (f, FieldRule { order, retired })
            })
            .collect();
        HeaderFallbackConfig { fields }
    }

    /// Every order back to its default chain; what `self` had beyond it is retired (the merge
    /// of [`Self::with_retirements_from`]).
    pub fn reset_orders(&self) -> HeaderFallbackConfig {
        HeaderFallbackConfig::default().with_retirements_from(self)
    }
}

const SETTINGS_KEY: &str = crate::settings::keys::CATALOG_HEADER_FALLBACKS;

/// The stored rules, expanded and validated. No row reads the default. A default keyword the
/// stored document lacks is put back (D14); an unknown field id is skipped with a warning. A
/// document that does not decode, or does not validate, is an error.
pub fn load(conn: &Connection) -> anyhow::Result<HeaderFallbackConfig> {
    use anyhow::Context;
    let Some(raw) = crate::db::get_setting(conn, SETTINGS_KEY)? else {
        return Ok(HeaderFallbackConfig::default());
    };
    let doc: serde_json::Value =
        serde_json::from_str(&raw).context("the stored FITS keyword rules are not JSON")?;
    let serde_json::Value::Object(root) = doc else {
        anyhow::bail!("the stored FITS keyword rules are not an object");
    };
    let mut cfg = HeaderFallbackConfig { fields: BTreeMap::new() };
    match root.get("fields") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(map)) => {
            for (id, value) in map {
                let Ok(field) = serde_json::from_value::<HeaderField>(serde_json::Value::String(id.clone()))
                else {
                    tracing::warn!(
                        key = SETTINGS_KEY,
                        reason = %id,
                        "unknown field id in stored FITS keyword rules; skipped"
                    );
                    continue;
                };
                let mut rule: FieldRule = serde_json::from_value(value.clone())
                    .with_context(|| format!("the stored rule of {id} does not decode"))?;
                rule.order = fill_missing_defaults(field, rule.order);
                cfg.fields.insert(field, rule);
            }
        }
        Some(_) => anyhow::bail!("the stored FITS keyword rules have a `fields` that is not an object"),
    }
    Ok(validate(&cfg)?)
}

/// [`load`] for a scan: a document that cannot be used is logged and the defaults are scanned
/// with, so a bad settings row never stops a scan.
pub fn load_for_scan(conn: &Connection) -> HeaderFallbackConfig {
    match load(conn) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(
                key = SETTINGS_KEY,
                error = %format!("{e:#}"),
                "stored FITS keyword rules do not decode; using the default"
            );
            HeaderFallbackConfig::default()
        }
    }
}

/// Persist the rules in their compact form; a document equal to the defaults (nothing
/// reordered, nothing retired) deletes the row.
pub fn store(conn: &Connection, cfg: &HeaderFallbackConfig) -> anyhow::Result<()> {
    let compact = cfg.compact();
    if compact.fields.is_empty() {
        crate::db::delete_setting(conn, SETTINGS_KEY)?;
    } else {
        crate::db::set_setting(conn, SETTINGS_KEY, &serde_json::to_string(&compact)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;


    fn cfg_with(field: HeaderField, order: &[&str]) -> HeaderFallbackConfig {
        let mut c = HeaderFallbackConfig::default();
        c.fields.insert(
            field,
            FieldRule { order: order.iter().map(|s| (*s).to_string()).collect(), retired: vec![] },
        );
        c
    }

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT)",
        )
        .unwrap();
        conn
    }

    fn stored(conn: &Connection) -> Option<String> {
        crate::db::get_setting(conn, SETTINGS_KEY).unwrap()
    }

    #[test]
    fn default_config_lists_every_field_with_its_default_chain() {
        let d = HeaderFallbackConfig::default();
        assert_eq!(d.fields.len(), 12);
        assert_eq!(d.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP"]);
        assert_eq!(d.order(HeaderField::Exptime), ["EXPTIME", "EXPOSURE"]);
        assert_eq!(d.order(HeaderField::Ypixsz), ["YPIXSZ", "PIXSIZE2"]);
        assert_eq!(d.order(HeaderField::Telescop), ["TELESCOP"]);
    }

    #[test]
    fn validate_accepts_the_default_and_expands_an_empty_document() {
        let d = HeaderFallbackConfig::default();
        assert_eq!(validate(&d).unwrap(), d);
        let empty = HeaderFallbackConfig { fields: BTreeMap::new() };
        assert_eq!(validate(&empty).unwrap(), d);
    }

    #[test]
    fn validate_refuses_a_dropped_default_keyword() {
        let err = validate(&cfg_with(HeaderField::CcdTemp, &["CCD-TEMP"])).unwrap_err();
        assert_eq!(
            err,
            RulesError::MissingDefault { field: HeaderField::CcdTemp, keyword: "DET-TEMP".into() }
        );
        assert_eq!(err.to_string(), "ccdTemp: the standard keyword DET-TEMP cannot be removed");
    }

    #[test]
    fn validate_refuses_an_invalid_keyword() {
        for bad in ["", "   ", "A=B"] {
            let err = validate(&cfg_with(HeaderField::Gain, &["GAIN", bad])).unwrap_err();
            assert_eq!(
                err,
                RulesError::InvalidKeyword { field: HeaderField::Gain, keyword: bad.into() }
            );
        }
        let msg = validate(&cfg_with(HeaderField::Gain, &["GAIN", "A=B"])).unwrap_err().to_string();
        assert_eq!(msg, "gain: \"A=B\" is not a valid keyword (empty or contains '=')");
    }

    #[test]
    fn validate_normalises_then_refuses_a_repeat() {
        let err =
            validate(&cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "ccd-temp "])).unwrap_err();
        assert_eq!(
            err,
            RulesError::Duplicate { field: HeaderField::CcdTemp, keyword: "CCD-TEMP".into() }
        );
        assert_eq!(err.to_string(), "ccdTemp: CCD-TEMP appears twice");
        let ok = validate(&cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "my-temp"])).unwrap();
        assert_eq!(ok.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP", "MY-TEMP"]);
    }

    #[test]
    fn validate_accepts_a_case_variant_of_a_default_keyword() {
        let ok = validate(&cfg_with(HeaderField::Exptime, &["exposure", " exptime"])).unwrap();
        assert_eq!(ok.order(HeaderField::Exptime), ["EXPOSURE", "EXPTIME"]);
    }

    #[test]
    fn unknown_field_id_does_not_deserialize() {
        assert!(serde_json::from_str::<HeaderFallbackConfig>(r#"{"fields":{"bogus":{"order":[]}}}"#).is_err());
    }

    #[test]
    fn validate_takes_order_keywords_out_of_retired() {
        let mut c = cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "MY-TEMP"]);
        c.fields.get_mut(&HeaderField::CcdTemp).unwrap().retired =
            vec!["old".into(), "MY-TEMP".into(), "OLD".into(), "A=B".into()];
        let v = validate(&c).unwrap();
        assert_eq!(v.retired(HeaderField::CcdTemp), ["OLD"]);
    }

    #[test]
    fn compact_keeps_only_changed_fields() {
        assert!(HeaderFallbackConfig::default().compact().fields.is_empty());
        let c = cfg_with(HeaderField::Gain, &["MY-GAIN", "GAIN"]).compact();
        assert_eq!(c.fields.keys().copied().collect::<Vec<_>>(), [HeaderField::Gain]);
        let mut r = HeaderFallbackConfig::default();
        r.fields.get_mut(&HeaderField::Filter).unwrap().retired = vec!["X".into()];
        assert_eq!(r.compact().fields.keys().copied().collect::<Vec<_>>(), [HeaderField::Filter]);
    }

    #[test]
    fn with_retirements_merges_newest_last_and_drops_what_is_back_in_the_order() {
        let mut prev = cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "A", "B"]);
        prev.fields.get_mut(&HeaderField::CcdTemp).unwrap().retired = vec!["OLD".into(), "A2".into()];
        let mut inc = cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "A2"]);
        inc.fields.get_mut(&HeaderField::CcdTemp).unwrap().retired = vec!["OLD".into()];
        let merged = inc.with_retirements_from(&prev);
        // A2 is back in the order; OLD repeated moves to its newest place; A and B are removed now.
        assert_eq!(merged.retired(HeaderField::CcdTemp), ["OLD", "A", "B"]);
        assert_eq!(merged.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP", "A2"]);
        assert_eq!(merged.fields.len(), 12);
    }

    #[test]
    fn fill_missing_defaults_follows_the_default_chain() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            fill_missing_defaults(HeaderField::CcdTemp, v(&["MY-TEMP", "CCD-TEMP"])),
            ["MY-TEMP", "CCD-TEMP", "DET-TEMP"]
        );
        assert_eq!(
            fill_missing_defaults(HeaderField::Exptime, v(&["X", "EXPOSURE"])),
            ["EXPTIME", "X", "EXPOSURE"]
        );
        assert_eq!(fill_missing_defaults(HeaderField::Gain, v(&[])), ["GAIN"]);
        // A case variant counts as present.
        assert_eq!(fill_missing_defaults(HeaderField::Gain, v(&["gain"])), ["gain"]);
    }

    #[test]
    fn defaults_are_not_written() {
        let conn = mem_conn();
        store(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(stored(&conn), None);
        // A stored row is deleted when the document returns to the defaults.
        store(&conn, &cfg_with(HeaderField::Gain, &["G2", "GAIN"])).unwrap();
        assert!(stored(&conn).is_some());
        store(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(stored(&conn), None);
    }

    #[test]
    fn store_writes_the_compact_form_and_load_expands_it() {
        let conn = mem_conn();
        store(&conn, &cfg_with(HeaderField::Gain, &["G2", "GAIN"])).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&stored(&conn).unwrap()).unwrap();
        assert_eq!(raw["fields"].as_object().unwrap().len(), 1);
        let loaded = load(&conn).unwrap();
        assert_eq!(loaded.fields.len(), 12);
        assert_eq!(loaded.order(HeaderField::Gain), ["G2", "GAIN"]);
    }

    #[test]
    fn load_inserts_a_default_keyword_the_stored_doc_lacks() {
        let conn = mem_conn();
        crate::db::set_setting(
            &conn,
            SETTINGS_KEY,
            r#"{"fields":{"ccdTemp":{"order":["MY-TEMP","CCD-TEMP"],"retired":[]},"exptime":{"order":["X","EXPOSURE"]}}}"#,
        )
        .unwrap();
        let c = load(&conn).unwrap();
        assert_eq!(c.order(HeaderField::CcdTemp), ["MY-TEMP", "CCD-TEMP", "DET-TEMP"]);
        assert_eq!(c.order(HeaderField::Exptime), ["EXPTIME", "X", "EXPOSURE"]);
    }

    #[test]
    fn load_skips_an_unknown_field_id_with_a_warning() {
        let conn = mem_conn();
        crate::db::set_setting(
            &conn,
            SETTINGS_KEY,
            r#"{"fields":{"future":{"order":["Z"]},"gain":{"order":["G2","GAIN"]}}}"#,
        )
        .unwrap();
        let c = load(&conn).unwrap();
        assert_eq!(c.order(HeaderField::Gain), ["G2", "GAIN"]);
        assert_eq!(c.fields.len(), 12);
    }

    #[test]
    fn load_with_no_row_is_the_default() {
        assert_eq!(load(&mem_conn()).unwrap(), HeaderFallbackConfig::default());
    }

    #[test]
    fn load_for_scan_falls_back_to_defaults_on_garbage() {
        let conn = mem_conn();
        for bad in ["not json", "[]", r#"{"fields":{"gain":{"order":["A=B","GAIN"]}}}"#] {
            crate::db::set_setting(&conn, SETTINGS_KEY, bad).unwrap();
            assert!(load(&conn).is_err(), "{bad}");
            assert_eq!(load_for_scan(&conn), HeaderFallbackConfig::default(), "{bad}");
        }
    }

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
