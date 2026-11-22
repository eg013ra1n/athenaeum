//! The CFA pattern enum and its one parser (moved from Athenaeum core's `fits_writer::keywords`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bayer {
    Rggb,
    Bggr,
    Gbrg,
    Grbg,
}
impl Bayer {
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
