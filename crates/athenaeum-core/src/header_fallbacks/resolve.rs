//! The one resolver: a rule (an ordered keyword list) applied to a stored header's key map.
//!
//! The FITS builder, the XISF builder, the metadata pane's snapshot, the override recomputation
//! and Apply all call [`resolve_all`], so a field cannot be read one way at scan and another way
//! on re-read.

use super::{FieldKind, HeaderField, HeaderFallbackConfig};
use crate::fits_parser::fits_header_reader::parse_fits_f64;
use crate::fits_parser::header_keys::{normalize_keyword, HeaderKeyMap};

/// A resolved header value.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Number(f64),
    Text(String),
}

/// Whether two values are the same catalog value: text exactly, numbers equal or within a
/// relative 1e-12 (a value that went through text and back).
pub fn same_value(a: &FieldValue, b: &FieldValue) -> bool {
    match (a, b) {
        (FieldValue::Text(x), FieldValue::Text(y)) => x == y,
        (FieldValue::Number(x), FieldValue::Number(y)) => {
            x == y || (x - y).abs() <= 1e-12 * x.abs().max(y.abs())
        }
        _ => false,
    }
}

/// The value one keyword gives for a field, `None` when the keyword is absent or its value has
/// the wrong type. A number is read with `parse_fits_f64` (Fortran `D` exponent included) and
/// must be finite (`NaN` / `inf` count as absent); text is trimmed and a blank one is absent.
pub fn resolve_keyword(field: HeaderField, keyword: &str, keys: &HeaderKeyMap) -> Option<FieldValue> {
    let key = normalize_keyword(keyword)?;
    let raw = keys.get(&key)?.trim();
    match field.kind() {
        FieldKind::Number => parse_fits_f64(raw)
            .filter(|n| n.is_finite())
            .map(FieldValue::Number),
        FieldKind::Text => {
            if raw.is_empty() {
                None
            } else {
                Some(FieldValue::Text(raw.to_string()))
            }
        }
    }
}

/// The first usable value of `order`, first keyword first.
pub fn resolve_field(field: HeaderField, order: &[String], keys: &HeaderKeyMap) -> Option<FieldValue> {
    order.iter().find_map(|k| resolve_keyword(field, k, keys))
}

/// The twelve rule-driven fields of a frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedFields {
    pub ccd_temp: Option<f64>,
    pub set_temp: Option<f64>,
    pub gain: Option<f64>,
    pub offset: Option<f64>,
    pub exptime: Option<f64>,
    pub focallen: Option<f64>,
    pub xpixsz: Option<f64>,
    pub ypixsz: Option<f64>,
    pub object: Option<String>,
    pub filter: Option<String>,
    pub instrume: Option<String>,
    pub telescop: Option<String>,
}

impl ResolvedFields {
    pub fn get(&self, f: HeaderField) -> Option<FieldValue> {
        let num = |v: Option<f64>| v.map(FieldValue::Number);
        let text = |v: &Option<String>| v.clone().map(FieldValue::Text);
        match f {
            HeaderField::CcdTemp => num(self.ccd_temp),
            HeaderField::SetTemp => num(self.set_temp),
            HeaderField::Gain => num(self.gain),
            HeaderField::Offset => num(self.offset),
            HeaderField::Exptime => num(self.exptime),
            HeaderField::Focallen => num(self.focallen),
            HeaderField::Xpixsz => num(self.xpixsz),
            HeaderField::Ypixsz => num(self.ypixsz),
            HeaderField::Object => text(&self.object),
            HeaderField::Filter => text(&self.filter),
            HeaderField::Instrume => text(&self.instrume),
            HeaderField::Telescop => text(&self.telescop),
        }
    }

    /// Store a value; a value of the wrong kind for the field stores `None`.
    pub fn set(&mut self, f: HeaderField, v: Option<FieldValue>) {
        let n = match &v {
            Some(FieldValue::Number(n)) => Some(*n),
            _ => None,
        };
        let t = match v {
            Some(FieldValue::Text(t)) => Some(t),
            _ => None,
        };
        match f {
            HeaderField::CcdTemp => self.ccd_temp = n,
            HeaderField::SetTemp => self.set_temp = n,
            HeaderField::Gain => self.gain = n,
            HeaderField::Offset => self.offset = n,
            HeaderField::Exptime => self.exptime = n,
            HeaderField::Focallen => self.focallen = n,
            HeaderField::Xpixsz => self.xpixsz = n,
            HeaderField::Ypixsz => self.ypixsz = n,
            HeaderField::Object => self.object = t,
            HeaderField::Filter => self.filter = t,
            HeaderField::Instrume => self.instrume = t,
            HeaderField::Telescop => self.telescop = t,
        }
    }
}

/// All twelve fields of a header under `rules`.
pub fn resolve_all(rules: &HeaderFallbackConfig, keys: &HeaderKeyMap) -> ResolvedFields {
    let mut out = ResolvedFields::default();
    for f in HeaderField::ALL {
        out.set(f, resolve_field(f, rules.order(f), keys));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> HeaderKeyMap {
        HeaderKeyMap::from_first_wins(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn order(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn ccd(keys: &HeaderKeyMap) -> Option<FieldValue> {
        resolve_field(HeaderField::CcdTemp, &order(&["CCD-TEMP", "DET-TEMP"]), keys)
    }

    #[test]
    fn resolver_takes_the_first_usable_keyword_in_order() {
        let k = keys(&[("CCD-TEMP", "-10"), ("DET-TEMP", "-5.5")]);
        assert_eq!(ccd(&k), Some(FieldValue::Number(-10.0)));
    }

    #[test]
    fn resolver_skips_a_value_of_the_wrong_type() {
        let k = keys(&[("CCD-TEMP", "n/a"), ("DET-TEMP", "-5.5")]);
        assert_eq!(ccd(&k), Some(FieldValue::Number(-5.5)));
    }

    #[test]
    fn resolver_treats_nan_inf_text_and_blank_as_absent() {
        for bad in ["NaN", "inf", "-INF", "n/a", "", "   "] {
            let k = keys(&[("CCD-TEMP", bad), ("DET-TEMP", "12")]);
            assert_eq!(ccd(&k), Some(FieldValue::Number(12.0)), "value {bad:?}");
        }
        let only_bad = keys(&[("CCD-TEMP", "NaN")]);
        assert_eq!(ccd(&only_bad), None);
    }

    #[test]
    fn user_first_order_wins_over_the_standard_keyword() {
        let k = keys(&[("MY-TEMP", "3"), ("CCD-TEMP", "-10"), ("DET-TEMP", "-5.5")]);
        let o = order(&["MY-TEMP", "CCD-TEMP", "DET-TEMP"]);
        assert_eq!(resolve_field(HeaderField::CcdTemp, &o, &k), Some(FieldValue::Number(3.0)));
    }

    #[test]
    fn default_keyword_demoted_to_the_end_is_tried_last() {
        let k = keys(&[("CCD-TEMP", "-10"), ("DET-TEMP", "-5.5")]);
        let o = order(&["DET-TEMP", "CCD-TEMP"]);
        assert_eq!(resolve_field(HeaderField::CcdTemp, &o, &k), Some(FieldValue::Number(-5.5)));
    }

    #[test]
    fn text_values_are_trimmed_and_blank_is_absent() {
        let k = keys(&[("FILTER", "  Ha  "), ("OBJECT", "  ")]);
        assert_eq!(
            resolve_keyword(HeaderField::Filter, "FILTER", &k),
            Some(FieldValue::Text("Ha".into()))
        );
        assert_eq!(resolve_keyword(HeaderField::Object, "OBJECT", &k), None);
    }

    #[test]
    fn fortran_d_notation_parses_on_every_path() {
        let k = keys(&[("EXPTIME", "1.2D+02")]);
        assert_eq!(
            resolve_keyword(HeaderField::Exptime, "EXPTIME", &k),
            Some(FieldValue::Number(120.0))
        );
    }

    #[test]
    fn a_case_or_space_variant_of_a_user_keyword_matches() {
        let k = keys(&[("ESO DET TEMP", "-7.25")]);
        assert_eq!(
            resolve_keyword(HeaderField::CcdTemp, " eso det temp ", &k),
            Some(FieldValue::Number(-7.25))
        );
        assert_eq!(resolve_keyword(HeaderField::CcdTemp, "A=B", &k), None);
    }

    #[test]
    fn same_value_compares_text_exactly_and_numbers_within_a_relative_epsilon() {
        use FieldValue::*;
        assert!(same_value(&Text("a".into()), &Text("a".into())));
        assert!(!same_value(&Text("a".into()), &Text("A".into())));
        assert!(same_value(&Number(-10.0), &Number(-10.0)));
        assert!(same_value(&Number(120.0), &Number(120.0 * (1.0 + 1e-13))));
        assert!(!same_value(&Number(120.0), &Number(120.001)));
        assert!(!same_value(&Number(1.0), &Text("1".into())));
    }

    #[test]
    fn resolve_all_reads_the_default_chains() {
        let k = keys(&[("DET-TEMP", "-5.5"), ("EXPOSURE", "120"), ("PIXSIZE1", "3.76"), ("OBJECT", "M42")]);
        let r = resolve_all(&HeaderFallbackConfig::default(), &k);
        assert_eq!(r.ccd_temp, Some(-5.5));
        assert_eq!(r.exptime, Some(120.0));
        assert_eq!(r.xpixsz, Some(3.76));
        assert_eq!(r.object.as_deref(), Some("M42"));
        assert_eq!(r.gain, None);
    }
}
