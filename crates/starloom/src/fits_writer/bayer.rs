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
