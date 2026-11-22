//! Export and import of the FITS keyword rules as a JSON file (spec §5.7, owner decision O3).
//!
//! The file holds only the fields whose order differs from the default chain; `retired` never
//! leaves the machine. Import replaces only the fields the file names and refuses a bad file as
//! a whole: [`plan_import`] returns the document that would be stored plus what changes, and
//! nothing reaches the settings until the user confirms and the frontend saves `config`.

use super::{
    fill_missing_defaults, validate, FieldRule, HeaderField, HeaderFallbackConfig, RulesError,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// The `format` member of the file.
pub const FILE_FORMAT: &str = "athenaeum.header-fallbacks";
/// The `version` member of the file; the only one this Athenaeum reads.
pub const FILE_VERSION: u64 = 1;
/// The largest file (or text) an import reads: 1 MiB.
pub const MAX_IMPORT_BYTES: usize = 1_048_576;
/// The file name the Export dialog proposes.
pub const DEFAULT_FILE_NAME: &str = "athenaeum-fits-keywords.json";

#[derive(Serialize)]
struct ExportFile<'a> {
    format: &'static str,
    version: u64,
    fields: BTreeMap<HeaderField, &'a [String]>,
}

/// The rules as pretty JSON: the fields whose order differs from the default chain, nothing
/// else (no `retired`, no paths, no catalog data).
pub fn export_text(cfg: &HeaderFallbackConfig) -> String {
    let fields = HeaderField::ALL
        .iter()
        .filter(|f| cfg.order(**f) != f.default_chain())
        .map(|&f| (f, cfg.order(f)))
        .collect();
    let file = ExportFile { format: FILE_FORMAT, version: FILE_VERSION, fields };
    // Serializing strings and a string-keyed map cannot fail.
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
    text.push('\n');
    text
}

// One field whose order an import changes, as the confirmation lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FieldOrderChange {
    pub field: HeaderField,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

// What importing a file would do: the document to store (expanded, `retired` merged) and the
// fields whose order differs from the current one, in `HeaderField::ALL` order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HeaderFallbackImportPlan {
    pub config: HeaderFallbackConfig,
    pub changes: Vec<FieldOrderChange>,
}

/// Why a file is refused. The messages name the problem; they reach the user as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    TooLarge(usize),
    NotJson(String),
    UnknownFormat(String),
    UnsupportedVersion(String),
    NoFields,
    UnknownField(String),
    NotAList(String),
    /// `index` is the entry's position in the list, counted from 0 (shown from 1).
    NotAString { field: String, index: usize },
    Rules(RulesError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::TooLarge(_) => write!(f, "the file is larger than 1 MB"),
            ImportError::NotJson(e) => write!(f, "not a JSON file: {e}"),
            ImportError::UnknownFormat(x) => write!(f, "not a FITS keyword file (format \"{x}\")"),
            ImportError::UnsupportedVersion(x) => {
                write!(f, "version {x} is not supported (this Athenaeum reads version 1)")
            }
            ImportError::NoFields => write!(f, "the file has no \"fields\" object"),
            ImportError::UnknownField(id) => write!(f, "unknown field \"{id}\""),
            ImportError::NotAList(id) => write!(f, "{id}: expected a list of keywords"),
            ImportError::NotAString { field, index } => {
                write!(f, "{field}: entry {} is not text", index + 1)
            }
            ImportError::Rules(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<RulesError> for ImportError {
    fn from(e: RulesError) -> Self {
        ImportError::Rules(e)
    }
}

/// Read a rules file against the current rules. The size is checked on `text.len()`, a leading
/// BOM is stripped, and any problem refuses the whole file: nothing of it reaches the result.
/// A named field's list gets a missing default keyword back (after its predecessor in the
/// default chain, the rule of §5.7) before it is validated.
pub fn plan_import(
    current: &HeaderFallbackConfig,
    text: &str,
) -> Result<HeaderFallbackImportPlan, ImportError> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge(text.len()));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let doc: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ImportError::NotJson(e.to_string()))?;
    let serde_json::Value::Object(root) = doc else {
        return Err(ImportError::NotJson("the top level is not an object".to_string()));
    };
    match root.get("format") {
        Some(serde_json::Value::String(s)) if s == FILE_FORMAT => {}
        Some(serde_json::Value::String(s)) => return Err(ImportError::UnknownFormat(s.clone())),
        Some(other) => return Err(ImportError::UnknownFormat(other.to_string())),
        None => return Err(ImportError::UnknownFormat("missing".to_string())),
    }
    match root.get("version") {
        Some(v) if v.as_u64() == Some(FILE_VERSION) => {}
        Some(v) => return Err(ImportError::UnsupportedVersion(v.to_string())),
        None => return Err(ImportError::UnsupportedVersion("missing".to_string())),
    }
    let Some(serde_json::Value::Object(fields)) = root.get("fields") else {
        return Err(ImportError::NoFields);
    };

    let mut next = current.clone();
    for (id, value) in fields {
        let field = serde_json::from_value::<HeaderField>(serde_json::Value::String(id.clone()))
            .map_err(|_| ImportError::UnknownField(id.clone()))?;
        let serde_json::Value::Array(list) = value else {
            return Err(ImportError::NotAList(id.clone()));
        };
        let mut order = Vec::with_capacity(list.len());
        for (index, entry) in list.iter().enumerate() {
            let serde_json::Value::String(s) = entry else {
                return Err(ImportError::NotAString { field: id.clone(), index });
            };
            order.push(s.clone());
        }
        let order = fill_missing_defaults(field, order);
        next.fields.insert(field, FieldRule { order, retired: current.retired(field).to_vec() });
    }
    // `validate` normalises every keyword and drops from `retired` whatever the order holds, so
    // a keyword the file only spells differently is not retired.
    let config = validate(&next.with_retirements_from(current))?;
    let changes = HeaderField::ALL
        .iter()
        .filter(|&&f| config.order(f) != current.order(f))
        .map(|&f| FieldOrderChange {
            field: f,
            before: current.order(f).to_vec(),
            after: config.order(f).to_vec(),
        })
        .collect();
    Ok(HeaderFallbackImportPlan { config, changes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(field: HeaderField, order: &[&str], retired: &[&str]) -> HeaderFallbackConfig {
        let mut c = HeaderFallbackConfig::default();
        c.fields.insert(
            field,
            FieldRule {
                order: order.iter().map(|s| (*s).to_string()).collect(),
                retired: retired.iter().map(|s| (*s).to_string()).collect(),
            },
        );
        c
    }

    fn file(fields: &str) -> String {
        format!(r#"{{"format":"athenaeum.header-fallbacks","version":1,"fields":{fields}}}"#)
    }

    fn refused(current: &HeaderFallbackConfig, text: &str) -> ImportError {
        plan_import(current, text).expect_err("the file must be refused")
    }

    #[test]
    fn export_writes_only_changed_fields_and_never_retired() {
        let c = cfg_with(HeaderField::CcdTemp, &["DET-TEMP", "CCD-TEMP", "MY-TEMP"], &["OLD"]);
        let text = export_text(&c);
        let got: serde_json::Value = serde_json::from_str(&text).unwrap();
        let want = serde_json::json!({
            "format": "athenaeum.header-fallbacks",
            "version": 1,
            "fields": { "ccdTemp": ["DET-TEMP", "CCD-TEMP", "MY-TEMP"] }
        });
        assert_eq!(got, want);
        assert!(!text.contains("OLD"), "{text}");
    }

    #[test]
    fn export_of_the_default_rules_names_no_field() {
        let got: serde_json::Value =
            serde_json::from_str(&export_text(&HeaderFallbackConfig::default())).unwrap();
        assert_eq!(got["fields"], serde_json::json!({}));
    }

    #[test]
    fn export_then_import_round_trips() {
        let mut c = cfg_with(HeaderField::CcdTemp, &["DET-TEMP", "CCD-TEMP", "MY-TEMP"], &[]);
        c.fields.insert(
            HeaderField::Object,
            FieldRule { order: vec!["MY-OBJ".into(), "OBJECT".into()], retired: vec![] },
        );
        let plan = plan_import(&HeaderFallbackConfig::default(), &export_text(&c)).unwrap();
        for f in HeaderField::ALL {
            assert_eq!(plan.config.order(f), c.order(f), "{}", f.id());
        }
        let changed: Vec<HeaderField> = plan.changes.iter().map(|c| c.field).collect();
        assert_eq!(changed, [HeaderField::CcdTemp, HeaderField::Object]);
    }

    #[test]
    fn import_replaces_only_named_fields_and_retires_lost_keywords() {
        let mut current = cfg_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "X"], &[]);
        current.fields.insert(
            HeaderField::Gain,
            FieldRule { order: vec!["MY-G".into(), "GAIN".into()], retired: vec!["OLD-G".into()] },
        );
        let plan = plan_import(&current, &file(r#"{"ccdTemp":["DET-TEMP","CCD-TEMP"]}"#)).unwrap();
        assert_eq!(plan.config.order(HeaderField::Gain), ["MY-G", "GAIN"]);
        assert_eq!(plan.config.retired(HeaderField::Gain), ["OLD-G"]);
        assert_eq!(plan.config.order(HeaderField::CcdTemp), ["DET-TEMP", "CCD-TEMP"]);
        assert_eq!(plan.config.retired(HeaderField::CcdTemp), ["X"]);
        assert_eq!(
            plan.changes,
            [FieldOrderChange {
                field: HeaderField::CcdTemp,
                before: vec!["CCD-TEMP".into(), "DET-TEMP".into(), "X".into()],
                after: vec!["DET-TEMP".into(), "CCD-TEMP".into()],
            }]
        );
    }

    #[test]
    fn import_of_the_same_order_in_other_case_is_no_change() {
        let current = cfg_with(HeaderField::CcdTemp, &["DET-TEMP", "CCD-TEMP"], &[]);
        let plan = plan_import(&current, &file(r#"{"ccdTemp":["det-temp"," ccd-temp "]}"#)).unwrap();
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert!(plan.config.retired(HeaderField::CcdTemp).is_empty());
    }

    #[test]
    fn import_appends_a_missing_default_after_its_predecessor() {
        let plan = plan_import(&HeaderFallbackConfig::default(), &file(r#"{"exptime":["EXPTIME","X"]}"#))
            .unwrap();
        assert_eq!(plan.config.order(HeaderField::Exptime), ["EXPTIME", "EXPOSURE", "X"]);
    }

    #[test]
    fn import_accepts_a_bom_and_crlf() {
        let text = "\u{feff}{\r\n \"format\": \"athenaeum.header-fallbacks\",\r\n \"version\": 1,\r\n \"fields\": {\r\n  \"gain\": [\"MY-G\", \"GAIN\"]\r\n }\r\n}\r\n";
        let plan = plan_import(&HeaderFallbackConfig::default(), text).unwrap();
        assert_eq!(plan.config.order(HeaderField::Gain), ["MY-G", "GAIN"]);
    }

    #[test]
    fn import_refuses_a_file_over_one_mib() {
        let text = " ".repeat(MAX_IMPORT_BYTES + 1);
        let e = refused(&HeaderFallbackConfig::default(), &text);
        assert_eq!(e, ImportError::TooLarge(MAX_IMPORT_BYTES + 1));
        assert!(e.to_string().contains("larger than 1 MB"), "{e}");
    }

    #[test]
    fn import_accepts_a_file_of_exactly_one_mib() {
        let mut text = file("{}");
        text.push_str(&" ".repeat(MAX_IMPORT_BYTES - text.len()));
        assert_eq!(text.len(), MAX_IMPORT_BYTES);
        assert!(plan_import(&HeaderFallbackConfig::default(), &text).is_ok());
    }

    #[test]
    fn import_refuses_bytes_that_are_not_json() {
        let e = refused(&HeaderFallbackConfig::default(), "\u{0}PK\u{3}");
        assert!(matches!(e, ImportError::NotJson(_)), "{e:?}");
        assert!(e.to_string().starts_with("not a JSON file: "), "{e}");
        let e = refused(&HeaderFallbackConfig::default(), "[1, 2]");
        assert!(matches!(e, ImportError::NotJson(_)), "{e:?}");
    }

    #[test]
    fn import_refuses_an_unknown_format() {
        let d = HeaderFallbackConfig::default();
        let e = refused(&d, r#"{"format":"something.else","version":1,"fields":{}}"#);
        assert_eq!(e, ImportError::UnknownFormat("something.else".into()));
        assert!(e.to_string().contains("something.else"), "{e}");
        let e = refused(&d, r#"{"version":1,"fields":{}}"#);
        assert!(matches!(e, ImportError::UnknownFormat(_)), "{e:?}");
    }

    #[test]
    fn import_refuses_version_two() {
        let d = HeaderFallbackConfig::default();
        let e = refused(&d, r#"{"format":"athenaeum.header-fallbacks","version":2,"fields":{}}"#);
        assert_eq!(e, ImportError::UnsupportedVersion("2".into()));
        assert!(e.to_string().contains("version 2"), "{e}");
    }

    #[test]
    fn import_refuses_a_missing_or_non_integer_version() {
        let d = HeaderFallbackConfig::default();
        for body in [
            r#"{"format":"athenaeum.header-fallbacks","fields":{}}"#,
            r#"{"format":"athenaeum.header-fallbacks","version":"1","fields":{}}"#,
            r#"{"format":"athenaeum.header-fallbacks","version":1.5,"fields":{}}"#,
            r#"{"format":"athenaeum.header-fallbacks","version":null,"fields":{}}"#,
        ] {
            let e = refused(&d, body);
            assert!(matches!(e, ImportError::UnsupportedVersion(_)), "{body}: {e:?}");
        }
    }

    #[test]
    fn import_refuses_a_file_without_fields() {
        let d = HeaderFallbackConfig::default();
        for body in [
            r#"{"format":"athenaeum.header-fallbacks","version":1}"#,
            r#"{"format":"athenaeum.header-fallbacks","version":1,"fields":[]}"#,
        ] {
            let e = refused(&d, body);
            assert_eq!(e, ImportError::NoFields, "{body}");
            assert!(e.to_string().contains("\"fields\""), "{e}");
        }
    }

    #[test]
    fn import_refuses_an_unknown_field() {
        let e = refused(&HeaderFallbackConfig::default(), &file(r#"{"ccdtemp":["CCD-TEMP"]}"#));
        assert_eq!(e, ImportError::UnknownField("ccdtemp".into()));
        assert!(e.to_string().contains("ccdtemp"), "{e}");
    }

    #[test]
    fn import_refuses_a_field_that_is_not_a_list() {
        let e = refused(&HeaderFallbackConfig::default(), &file(r#"{"gain":"GAIN"}"#));
        assert_eq!(e, ImportError::NotAList("gain".into()));
        assert!(e.to_string().contains("gain"), "{e}");
    }

    #[test]
    fn import_refuses_a_non_string_keyword() {
        let e = refused(&HeaderFallbackConfig::default(), &file(r#"{"gain":["GAIN",7]}"#));
        assert_eq!(e, ImportError::NotAString { field: "gain".into(), index: 1 });
        assert_eq!(e.to_string(), "gain: entry 2 is not text");
    }

    #[test]
    fn import_refuses_an_invalid_keyword() {
        for bad in [r#""""#, r#""A=B""#, r#""   ""#] {
            let body = file(&format!(r#"{{"gain":["GAIN",{bad}]}}"#));
            let e = refused(&HeaderFallbackConfig::default(), &body);
            assert!(
                matches!(e, ImportError::Rules(RulesError::InvalidKeyword { field: HeaderField::Gain, .. })),
                "{bad}: {e:?}"
            );
            assert!(e.to_string().contains("gain"), "{e}");
        }
    }

    #[test]
    fn import_refuses_a_repeated_keyword() {
        let e = refused(
            &HeaderFallbackConfig::default(),
            &file(r#"{"ccdTemp":["CCD-TEMP","DET-TEMP","ccd-temp"]}"#),
        );
        assert_eq!(
            e,
            ImportError::Rules(RulesError::Duplicate {
                field: HeaderField::CcdTemp,
                keyword: "CCD-TEMP".into()
            })
        );
        assert!(e.to_string().contains("CCD-TEMP"), "{e}");
    }

    #[test]
    fn a_refusal_carries_no_part_of_the_file() {
        // The first field is fine, the second is bad: the whole file is refused.
        let e = refused(
            &HeaderFallbackConfig::default(),
            &file(r#"{"ccdTemp":["DET-TEMP","CCD-TEMP"],"gain":["GAIN","A=B"]}"#),
        );
        assert!(matches!(e, ImportError::Rules(_)), "{e:?}");
    }
}
