//! The Bayer color-filter-array pattern enum and its one parser of stored pattern text.

/// The 2x2 Bayer color filter array of a one-shot-color sensor, named by the filter colors of the top-left 2x2 block
/// read row by row (R red, G green, B blue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bayer {
    /// Red, green on the first row; green, blue on the second.
    Rggb,
    /// Blue, green on the first row; green, red on the second.
    Bggr,
    /// Green, blue on the first row; red, green on the second.
    Gbrg,
    /// Green, red on the first row; blue, green on the second.
    Grbg,
}
impl Bayer {
    /// The canonical upper-case spelling of the pattern (`"RGGB"`, `"BGGR"`, `"GBRG"` or `"GRBG"`), the text of a
    /// `BAYERPAT` card.
    pub fn as_str(&self) -> &'static str {
        match self {
            Bayer::Rggb => "RGGB",
            Bayer::Bggr => "BGGR",
            Bayer::Gbrg => "GBRG",
            Bayer::Grbg => "GRBG",
        }
    }

    /// The one reader of a stored CFA pattern string. `frames.bayerpat` keeps
    /// whatever the source parser yielded, and quoting/case vary by writer, so
    /// normalize before matching. `None` = a value we cannot vouch for; each
    /// call site decides whether that deserves a warning (emitting a master's
    /// BAYERPAT card does, measuring per-channel statistics does not — the same
    /// inputs go through both).
    ///
    /// Lives here beside the enum rather than in `calibration_library`: the
    /// light-calibration side reads it from ungated code (the header/card
    /// builders), and `fits_writer` is the ungated module both sides already
    /// depend on.
    pub fn parse(s: &str) -> Option<Bayer> {
        match s
            .trim()
            .trim_matches('\'')
            .trim()
            .to_ascii_uppercase()
            .as_str()
        {
            "RGGB" => Some(Bayer::Rggb),
            "BGGR" => Some(Bayer::Bggr),
            "GBRG" => Some(Bayer::Gbrg),
            "GRBG" => Some(Bayer::Grbg),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stored-column spellings `Bayer::parse` has to survive: quoted (FITS
    /// card text kept verbatim by the parser), padded, and lower-cased. Anything
    /// it cannot vouch for is `None`, never a guess.
    #[test]
    fn bayer_parse_normalizes_stored_spellings() {
        for pattern in [Bayer::Rggb, Bayer::Bggr, Bayer::Gbrg, Bayer::Grbg] {
            let canonical = pattern.as_str();
            for spelling in [
                canonical.to_string(),
                canonical.to_ascii_lowercase(),
                format!("'{canonical}'"),
                format!("  '{canonical}  '  "),
            ] {
                assert_eq!(
                    Bayer::parse(&spelling),
                    Some(pattern),
                    "spelling {spelling:?}"
                );
            }
        }
        for bad in ["", "   ", "RGB", "RGGBX", "''", "MONO"] {
            assert_eq!(Bayer::parse(bad), None, "{bad:?} must not parse");
        }
    }
}
