//! Resolve right ascension in degrees without inferring hours from magnitude.

/// Resolve a numeric RA card to degrees, normalized to [0, 360).
///
/// `unit` is the RA card's comment; explicit degree/hour labels take precedence.
/// Hours must be in [0, 24) and convert as `degrees = 15 * hours`. Conflicting
/// labels or a nonfinite `ra` return `None` without consulting the fallbacks.
///
/// Without unit labels, a parseable `objctra` (sexagesimal hours) takes priority;
/// it is not checked for agreement with the numeric card. Otherwise, values
/// outside [0, 24) are treated as degrees. Values inside that ambiguous range
/// use a finite `wcs` RA in degrees, or return `None` if none is available.
/// This resolves units only; it does not transform coordinate frames or epochs.
pub fn resolve(ra: f64, unit: &str, objctra: Option<&str>, wcs: Option<f64>) -> Option<f64> {
    if !ra.is_finite() {
        return None;
    }
    let words: Vec<_> = unit
        .split(|c: char| !c.is_ascii_alphabetic())
        .map(str::to_ascii_lowercase)
        .collect();
    let degrees = words
        .iter()
        .any(|w| matches!(w.as_str(), "degree" | "degrees" | "deg"));
    let hours = words
        .iter()
        .any(|w| matches!(w.as_str(), "hour" | "hours" | "hr" | "hrs" | "h"));
    if degrees && !hours {
        return Some(crate::coordinates::normalize_ra(ra));
    }
    if hours && !degrees {
        return (0.0..24.0).contains(&ra).then_some(ra * 15.0);
    }
    if degrees && hours {
        return None;
    }
    if let Some(value) = objctra
        .and_then(|s| crate::coordinates::parse_ra_sexagesimal(s).ok())
        .filter(|v| v.is_finite())
    {
        return Some(value);
    }
    if !(0.0..24.0).contains(&ra) {
        return Some(crate::coordinates::normalize_ra(ra));
    }
    wcs.filter(|v| v.is_finite())
        .map(crate::coordinates::normalize_ra)
}

/// Extract only the RA card's unit comment from stored FITS text or XISF XML.
/// Other cards are ignored. Returns an empty string when no RA comment is found;
/// malformed XML is logged and stops the scan, while invalid attributes are skipped.
pub fn comment(text: &str) -> String {
    if text.trim_start().starts_with('<') {
        use quick_xml::{events::Event, Reader};
        let mut reader = Reader::from_str(text);
        loop {
            match reader.read_event() {
                Ok(Event::Empty(e)) | Ok(Event::Start(e))
                    if e.name().as_ref() == b"FITSKeyword" =>
                {
                    let mut name = String::new();
                    let mut comment = String::new();
                    for attribute in e.attributes() {
                        let a = match attribute {
                            Ok(a) => a,
                            Err(error) => {
                                tracing::warn!(%error, "invalid XISF coordinate attribute");
                                continue;
                            }
                        };
                        let value = match a.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
                            Ok(value) => value.into_owned(),
                            Err(error) => {
                                tracing::warn!(%error, "invalid XISF coordinate attribute value");
                                continue;
                            }
                        };
                        match a.key.as_ref() {
                            b"name" => name = value,
                            b"comment" => comment = value,
                            _ => {}
                        }
                    }
                    if name.eq_ignore_ascii_case("RA") {
                        return comment;
                    }
                }
                Ok(Event::Eof) => break,
                Err(error) => {
                    tracing::warn!(%error, "could not read XISF coordinate units");
                    break;
                }
                _ => {}
            }
        }
    } else {
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("RA") {
                    return value
                        .split_once('/')
                        .map(|(_, c)| c.trim().to_owned())
                        .unwrap_or_default();
                }
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_degrees_hours_and_ambiguity() {
        assert_eq!(
            resolve(10.487505, "Object Right Ascension in degrees", None, None),
            Some(10.487505)
        );
        assert_eq!(resolve(10.0, "hours", None, None), Some(150.0));
        assert_eq!(resolve(10.0, "", None, None), None);
        assert_eq!(
            resolve(10.0, "", None, Some(10.0868529347)),
            Some(10.0868529347)
        );
        assert_eq!(resolve(0.0, "deg", None, None), Some(0.0));
        assert_eq!(resolve(f64::NAN, "deg", None, None), None);
        assert_eq!(resolve(10.0, "degrees or hours", None, None), None);
    }
    #[test]
    fn fits_and_xisf_comments_match() {
        for text in ["RA      = 10.487505 / Object Right Ascension in degrees", "<?xml version=\"1.0\"?><xisf><FITSKeyword name=\"RA\" value=\"10.487505\" comment=\"Object Right Ascension in degrees\"/></xisf>"] {
            assert_eq!(resolve(10.487505,&comment(text),None,None),Some(10.487505));
        }
    }
}
