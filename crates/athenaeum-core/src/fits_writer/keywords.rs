//! Typed FITS keyword vocabulary — canonical, standards-based header values.
//! Sources: SBFITSEXT 1.0 (IMAGETYP/EXPTIME/CCD-TEMP/…), NINA conventions
//! (GAIN/OFFSET/BAYERPAT/ROWORDER), WBPP master detection ("master" substring),
//! FITS 4.0 (dates, unit-bracket comments). Custom namespace: ATH_* (<= 8 chars).

use chrono::{DateTime, Utc};

use super::card::{Card, CardValue, FitsWriteError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameKind {
    Light,
    Dark,
    Bias,
    Flat,
    DarkFlat,
    MasterLight,
    MasterDark,
    MasterBias,
    MasterFlat,
    MasterDarkFlat,
}

impl FrameKind {
    pub fn imagetyp(&self) -> &'static str {
        match self {
            FrameKind::Light => "Light Frame",
            FrameKind::Dark => "Dark Frame",
            FrameKind::Bias => "Bias Frame",
            FrameKind::Flat => "Flat Field",
            FrameKind::DarkFlat => "Dark Flat",
            FrameKind::MasterLight => "Master Light",
            FrameKind::MasterDark => "Master Dark",
            FrameKind::MasterBias => "Master Bias",
            FrameKind::MasterFlat => "Master Flat",
            FrameKind::MasterDarkFlat => "Master Dark Flat",
        }
    }
}

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
    /// light-calibration side reads it from ungated code (`db::light_calibrations`
    /// derives CFA staleness), and `fits_writer` is the ungated module both
    /// sides already depend on.
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

pub fn ra_to_sexagesimal(ra_deg: f64) -> String {
    // Round to integer milliseconds of time first, then decompose — seconds
    // can never round to 60 and the 24h boundary wraps to 00 00 00.000.
    const DAY_MS: u64 = 24 * 3_600_000;
    let total_ms = ((ra_deg.rem_euclid(360.0) / 15.0) * 3_600_000.0).round() as u64 % DAY_MS;
    let h = total_ms / 3_600_000;
    let m = (total_ms % 3_600_000) / 60_000;
    let s = (total_ms % 60_000) as f64 / 1000.0;
    format!("{h:02} {m:02} {s:06.3}")
}

pub fn dec_to_sexagesimal(dec_deg: f64) -> String {
    // Round to integer centiarcseconds first — carry propagates through
    // minutes/degrees naturally (+90 00 00.00 at the pole is valid).
    // Note: -0.0 formats as "-00 00 00.00" under is_sign_negative(); this is
    // accepted as consistent with signed-zero semantics elsewhere in Rust.
    let sign = if dec_deg.is_sign_negative() { '-' } else { '+' };
    let total_cs = (dec_deg.abs() * 360_000.0).round() as u64;
    let d = total_cs / 360_000;
    let m = (total_cs % 360_000) / 6_000;
    let s = (total_cs % 6_000) as f64 / 100.0;
    format!("{sign}{d:02} {m:02} {s:05.2}")
}

pub struct HeaderBuilder {
    cards: Vec<Card>,
    err: Option<FitsWriteError>,
}

impl HeaderBuilder {
    pub fn new(kind: FrameKind) -> Self {
        let mut b = HeaderBuilder {
            cards: Vec::new(),
            err: None,
        };
        b.push_str("IMAGETYP", kind.imagetyp(), "type of image");
        b
    }

    fn push(&mut self, kw: &str, v: CardValue, comment: &str) {
        if self.err.is_some() {
            return;
        }
        match Card::new(kw, v) {
            Ok(c) => self.cards.push(if comment.is_empty() {
                c
            } else {
                c.with_comment(comment)
            }),
            Err(e) => self.err = Some(e),
        }
    }
    fn push_str(&mut self, kw: &str, v: &str, comment: &str) {
        self.push(kw, CardValue::Str(v.to_string()), comment);
    }

    pub fn swcreate(mut self, app_version: &str) -> Self {
        self.push_str(
            "SWCREATE",
            &format!("Athenaeum {app_version}"),
            "software that created this file",
        );
        self
    }
    pub fn exptime(mut self, secs: f64) -> Self {
        self.push("EXPTIME", CardValue::Real(secs), "[s] exposure duration");
        self
    }
    pub fn date_obs(mut self, t: DateTime<Utc>) -> Self {
        self.push_str(
            "DATE-OBS",
            &t.format("%Y-%m-%dT%H:%M:%S%.3f").to_string(),
            "UTC observation start/midpoint",
        );
        self
    }
    pub fn ccd_temp(mut self, c: f64) -> Self {
        self.push("CCD-TEMP", CardValue::Real(c), "[degC] sensor temperature");
        self
    }
    pub fn set_temp(mut self, c: f64) -> Self {
        self.push("SET-TEMP", CardValue::Real(c), "[degC] cooling setpoint");
        self
    }
    pub fn gain(mut self, g: i64) -> Self {
        self.push("GAIN", CardValue::Integer(g), "camera gain setting");
        self
    }
    pub fn offset(mut self, o: i64) -> Self {
        self.push("OFFSET", CardValue::Integer(o), "camera offset setting");
        self
    }
    pub fn egain(mut self, e: f64) -> Self {
        self.push("EGAIN", CardValue::Real(e), "[e-/ADU] electronic gain");
        self
    }
    pub fn binning(mut self, x: i64, y: i64) -> Self {
        self.push("XBINNING", CardValue::Integer(x), "binning factor X");
        self.push("YBINNING", CardValue::Integer(y), "binning factor Y");
        self
    }
    pub fn pixel_size(mut self, x_um: f64, y_um: f64) -> Self {
        self.push(
            "XPIXSZ",
            CardValue::Real(x_um),
            "[um] pixel width after binning",
        );
        self.push(
            "YPIXSZ",
            CardValue::Real(y_um),
            "[um] pixel height after binning",
        );
        self
    }
    /// BAYERPAT alone. The offsets are deliberately separate: a source that
    /// declares a pattern but no phase must yield a pattern-only header —
    /// XBAYROFF=0 is a real phase claim, not a neutral default, and a wrong
    /// one swaps colour channels on debayer.
    pub fn bayer_pattern(mut self, b: Bayer) -> Self {
        self.push_str("BAYERPAT", b.as_str(), "Bayer color pattern");
        self
    }
    pub fn bayer_x_offset(mut self, xoff: i64) -> Self {
        self.push("XBAYROFF", CardValue::Integer(xoff), "Bayer X offset");
        self
    }
    pub fn bayer_y_offset(mut self, yoff: i64) -> Self {
        self.push("YBAYROFF", CardValue::Integer(yoff), "Bayer Y offset");
        self
    }
    /// Convenience for callers that know all three (pattern + both offsets).
    pub fn bayer(self, b: Bayer, xoff: i64, yoff: i64) -> Self {
        self.bayer_pattern(b)
            .bayer_x_offset(xoff)
            .bayer_y_offset(yoff)
    }
    pub fn instrume(mut self, v: &str) -> Self {
        self.push_str("INSTRUME", v, "camera");
        self
    }
    pub fn telescop(mut self, v: &str) -> Self {
        self.push_str("TELESCOP", v, "telescope");
        self
    }
    pub fn focallen(mut self, mm: f64) -> Self {
        self.push("FOCALLEN", CardValue::Real(mm), "[mm] focal length");
        self
    }
    pub fn filter(mut self, v: &str) -> Self {
        self.push_str("FILTER", v, "filter name");
        self
    }
    pub fn object(mut self, v: &str) -> Self {
        self.push_str("OBJECT", v, "target name");
        self
    }
    /// ROWORDER verbatim. Callers pass the canonical `TOP-DOWN` / `BOTTOM-UP`
    /// spellings; validating the value is the caller's job (a master's row
    /// order is decided by its members, not by this builder).
    pub fn roworder(mut self, v: &str) -> Self {
        self.push_str("ROWORDER", v, "image row order");
        self
    }
    pub fn calstat(mut self, flags: &str) -> Self {
        self.push_str("CALSTAT", flags, "calibration state (B/D/F)");
        self
    }
    pub fn pedestal(mut self, p: i64) -> Self {
        self.push(
            "PEDESTAL",
            CardValue::Integer(p),
            "add to ADU for zero base",
        );
        self
    }

    pub fn ath_src(mut self, uuid: &str) -> Self {
        self.push_str("ATH_SRC", uuid, "source calibration_set uuid");
        self
    }
    pub fn ath_n(mut self, n: u32) -> Self {
        self.push(
            "ATH_N",
            CardValue::Integer(n as i64),
            "number of integrated frames",
        );
        self
    }
    pub fn ath_rej(mut self, v: &str) -> Self {
        self.push_str("ATH_REJ", v, "rejection algorithm");
        self
    }
    pub fn ath_ver(mut self, v: &str) -> Self {
        self.push_str("ATH_VER", v, "athenaeum version");
        self
    }
    pub fn ath_hsh(mut self, v: &str) -> Self {
        self.push_str("ATH_HSH", v, "xxh3 of member hash list");
        self
    }
    pub fn ath_temp_span(mut self, min_c: f64, max_c: f64) -> Self {
        self.push(
            "ATH_TMIN",
            CardValue::Real(min_c),
            "[degC] min member CCD-TEMP",
        );
        self.push(
            "ATH_TMAX",
            CardValue::Real(max_c),
            "[degC] max member CCD-TEMP",
        );
        self
    }

    pub fn custom(mut self, c: Card) -> Self {
        if self.err.is_some() {
            return self;
        }
        self.cards.push(c);
        self
    }

    pub fn build(self) -> Result<Vec<Card>, FitsWriteError> {
        match self.err {
            Some(e) => Err(e),
            None => Ok(self.cards),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ImageType;

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

    #[test]
    fn every_canonical_imagetyp_roundtrips_through_our_parser() {
        let all = [
            (FrameKind::Light, ImageType::Light),
            (FrameKind::Dark, ImageType::Dark),
            (FrameKind::Bias, ImageType::Bias),
            (FrameKind::Flat, ImageType::Flat),
            (FrameKind::DarkFlat, ImageType::DarkFlat),
            (FrameKind::MasterLight, ImageType::MasterLight),
            (FrameKind::MasterDark, ImageType::MasterDark),
            (FrameKind::MasterBias, ImageType::MasterBias),
            (FrameKind::MasterFlat, ImageType::MasterFlat),
            (FrameKind::MasterDarkFlat, ImageType::MasterDarkFlat),
        ];
        for (kind, expected) in all {
            let parsed = ImageType::from_str(kind.imagetyp());
            assert_eq!(parsed, Some(expected), "IMAGETYP {:?}", kind.imagetyp());
        }
        // ImageType::from_str(s: &str) -> Option<Self> — verified at models.rs:111.
    }

    #[test]
    fn master_values_contain_master_substring_for_wbpp() {
        for k in [
            FrameKind::MasterLight,
            FrameKind::MasterDark,
            FrameKind::MasterBias,
            FrameKind::MasterFlat,
            FrameKind::MasterDarkFlat,
        ] {
            assert!(k.imagetyp().to_lowercase().contains("master"));
        }
    }

    #[test]
    fn sexagesimal_reparse_within_arcsec() {
        // M31: RA 10.684708°, DEC +41.269065°
        let (ra_s, dec_s) = (ra_to_sexagesimal(10.684708), dec_to_sexagesimal(41.269065));
        assert_eq!(ra_s.split(' ').count(), 3, "{ra_s}");
        assert!(dec_s.starts_with('+'), "{dec_s}");
        // reparse and compare
        let parts: Vec<f64> = ra_s.split(' ').map(|p| p.parse().unwrap()).collect();
        let ra_back = (parts[0] + parts[1] / 60.0 + parts[2] / 3600.0) * 15.0;
        assert!(
            (ra_back - 10.684708).abs() < 0.001 / 3600.0 * 15.0,
            "{ra_s} -> {ra_back}"
        );
        let dparts: Vec<f64> = dec_s[1..].split(' ').map(|p| p.parse().unwrap()).collect();
        let dec_back = dparts[0] + dparts[1] / 60.0 + dparts[2] / 3600.0;
        assert!(
            (dec_back - 41.269065).abs() < 0.01 / 3600.0,
            "{dec_s} -> {dec_back}"
        );
    }

    #[test]
    fn negative_dec_sign() {
        assert!(dec_to_sexagesimal(-16.716).starts_with('-'));
    }

    #[test]
    fn sexagesimal_wrap_boundaries() {
        assert_eq!(ra_to_sexagesimal(359.9999999999), "00 00 00.000");
        assert_eq!(ra_to_sexagesimal(360.0), "00 00 00.000");
        assert_eq!(dec_to_sexagesimal(89.999999999), "+90 00 00.00");
        assert_eq!(ra_to_sexagesimal(0.0), "00 00 00.000");
        assert_eq!(dec_to_sexagesimal(0.0), "+00 00 00.00");
    }

    #[test]
    fn ath_keywords_are_all_within_8_chars() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ath_src("abc")
            .ath_n(30)
            .ath_rej("sigma3.0/2.5")
            .ath_ver("0.2.4")
            .ath_hsh("deadbeef")
            .ath_temp_span(-10.6, -9.8)
            .build()
            .unwrap();
        for c in &cards {
            assert!(c.keyword.len() <= 8, "{}", c.keyword);
        }
        assert!(cards.iter().any(|c| c.keyword == "ATH_TMIN"));
    }

    #[test]
    fn builder_emits_units_in_comments() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ccd_temp(-10.0)
            .exptime(300.0)
            .build()
            .unwrap();
        let ccd = cards.iter().find(|c| c.keyword == "CCD-TEMP").unwrap();
        assert!(ccd.comment.as_deref().unwrap_or("").contains("[degC]"));
    }
}
