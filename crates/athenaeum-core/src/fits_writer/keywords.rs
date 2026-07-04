//! Typed FITS keyword vocabulary — canonical, standards-based header values.
//! Sources: SBFITSEXT 1.0 (IMAGETYP/EXPTIME/CCD-TEMP/…), NINA conventions
//! (GAIN/OFFSET/BAYERPAT/ROWORDER), WBPP master detection ("master" substring),
//! FITS 4.0 (dates, unit-bracket comments). Custom namespace: ATH_* (<= 8 chars).

use chrono::{DateTime, Utc};

use super::card::{Card, CardValue, FitsWriteError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameKind {
    Light, Dark, Bias, Flat, DarkFlat,
    MasterLight, MasterDark, MasterBias, MasterFlat, MasterDarkFlat,
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

#[derive(Debug, Clone, Copy)]
pub enum Bayer { Rggb, Bggr, Gbrg, Grbg }
impl Bayer {
    pub fn as_str(&self) -> &'static str {
        match self { Bayer::Rggb => "RGGB", Bayer::Bggr => "BGGR", Bayer::Gbrg => "GBRG", Bayer::Grbg => "GRBG" }
    }
}

pub fn ra_to_sexagesimal(ra_deg: f64) -> String {
    let total_h = ra_deg.rem_euclid(360.0) / 15.0;
    let h = total_h.floor();
    let total_m = (total_h - h) * 60.0;
    let m = total_m.floor();
    let s = (total_m - m) * 60.0;
    format!("{:02} {:02} {:06.3}", h as u32, m as u32, s)
}

pub fn dec_to_sexagesimal(dec_deg: f64) -> String {
    let sign = if dec_deg < 0.0 { '-' } else { '+' };
    let a = dec_deg.abs();
    let d = a.floor();
    let total_m = (a - d) * 60.0;
    let m = total_m.floor();
    let s = (total_m - m) * 60.0;
    format!("{sign}{:02} {:02} {:05.2}", d as u32, m as u32, s)
}

pub struct HeaderBuilder {
    cards: Vec<Card>,
    err: Option<FitsWriteError>,
}

impl HeaderBuilder {
    pub fn new(kind: FrameKind) -> Self {
        let mut b = HeaderBuilder { cards: Vec::new(), err: None };
        b.push_str("IMAGETYP", kind.imagetyp(), "type of image");
        b
    }

    fn push(&mut self, kw: &str, v: CardValue, comment: &str) {
        if self.err.is_some() { return; }
        match Card::new(kw, v) {
            Ok(c) => self.cards.push(if comment.is_empty() { c } else { c.with_comment(comment) }),
            Err(e) => self.err = Some(e),
        }
    }
    fn push_str(&mut self, kw: &str, v: &str, comment: &str) {
        self.push(kw, CardValue::Str(v.to_string()), comment);
    }

    pub fn swcreate(mut self, app_version: &str) -> Self {
        self.push_str("SWCREATE", &format!("Athenaeum {app_version}"), "software that created this file"); self
    }
    pub fn exptime(mut self, secs: f64) -> Self {
        self.push("EXPTIME", CardValue::Real(secs), "[s] exposure duration"); self
    }
    pub fn date_obs(mut self, t: DateTime<Utc>) -> Self {
        self.push_str("DATE-OBS", &t.format("%Y-%m-%dT%H:%M:%S%.3f").to_string(), "UTC observation start/midpoint"); self
    }
    pub fn ccd_temp(mut self, c: f64) -> Self {
        self.push("CCD-TEMP", CardValue::Real(c), "[degC] sensor temperature"); self
    }
    pub fn set_temp(mut self, c: f64) -> Self {
        self.push("SET-TEMP", CardValue::Real(c), "[degC] cooling setpoint"); self
    }
    pub fn gain(mut self, g: i64) -> Self { self.push("GAIN", CardValue::Integer(g), "camera gain setting"); self }
    pub fn offset(mut self, o: i64) -> Self { self.push("OFFSET", CardValue::Integer(o), "camera offset setting"); self }
    pub fn egain(mut self, e: f64) -> Self { self.push("EGAIN", CardValue::Real(e), "[e-/ADU] electronic gain"); self }
    pub fn binning(mut self, x: i64, y: i64) -> Self {
        self.push("XBINNING", CardValue::Integer(x), "binning factor X");
        self.push("YBINNING", CardValue::Integer(y), "binning factor Y"); self
    }
    pub fn pixel_size(mut self, x_um: f64, y_um: f64) -> Self {
        self.push("XPIXSZ", CardValue::Real(x_um), "[um] pixel width after binning");
        self.push("YPIXSZ", CardValue::Real(y_um), "[um] pixel height after binning"); self
    }
    pub fn bayer(mut self, b: Bayer, xoff: i64, yoff: i64) -> Self {
        self.push_str("BAYERPAT", b.as_str(), "Bayer color pattern");
        self.push("XBAYROFF", CardValue::Integer(xoff), "Bayer X offset");
        self.push("YBAYROFF", CardValue::Integer(yoff), "Bayer Y offset"); self
    }
    pub fn radec(mut self, ra_deg: f64, dec_deg: f64) -> Self {
        self.push("RA", CardValue::Real(ra_deg), "[deg] right ascension");
        self.push("DEC", CardValue::Real(dec_deg), "[deg] declination");
        self.push_str("OBJCTRA", &ra_to_sexagesimal(ra_deg), "RA of image center, HH MM SS.SSS");
        self.push_str("OBJCTDEC", &dec_to_sexagesimal(dec_deg), "DEC of image center, +DD MM SS.SS"); self
    }
    pub fn instrume(mut self, v: &str) -> Self { self.push_str("INSTRUME", v, "camera"); self }
    pub fn telescop(mut self, v: &str) -> Self { self.push_str("TELESCOP", v, "telescope"); self }
    pub fn focallen(mut self, mm: f64) -> Self { self.push("FOCALLEN", CardValue::Real(mm), "[mm] focal length"); self }
    pub fn filter(mut self, v: &str) -> Self { self.push_str("FILTER", v, "filter name"); self }
    pub fn object(mut self, v: &str) -> Self { self.push_str("OBJECT", v, "target name"); self }
    pub fn roworder_top_down(mut self) -> Self { self.push_str("ROWORDER", "TOP-DOWN", "image row order"); self }
    pub fn calstat(mut self, flags: &str) -> Self { self.push_str("CALSTAT", flags, "calibration state (B/D/F)"); self }
    pub fn pedestal(mut self, p: i64) -> Self { self.push("PEDESTAL", CardValue::Integer(p), "add to ADU for zero base"); self }

    pub fn ath_src(mut self, uuid: &str) -> Self { self.push_str("ATH_SRC", uuid, "source calibration_set uuid"); self }
    pub fn ath_n(mut self, n: u32) -> Self { self.push("ATH_N", CardValue::Integer(n as i64), "number of integrated frames"); self }
    pub fn ath_rej(mut self, v: &str) -> Self { self.push_str("ATH_REJ", v, "rejection algorithm"); self }
    pub fn ath_ver(mut self, v: &str) -> Self { self.push_str("ATH_VER", v, "athenaeum version"); self }
    pub fn ath_hsh(mut self, v: &str) -> Self { self.push_str("ATH_HSH", v, "xxh3 of member hash list"); self }
    pub fn ath_temp_span(mut self, min_c: f64, max_c: f64) -> Self {
        self.push("ATH_TMIN", CardValue::Real(min_c), "[degC] min member CCD-TEMP");
        self.push("ATH_TMAX", CardValue::Real(max_c), "[degC] max member CCD-TEMP"); self
    }

    pub fn custom(mut self, c: Card) -> Self { self.cards.push(c); self }

    pub fn build(self) -> Result<Vec<Card>, FitsWriteError> {
        match self.err { Some(e) => Err(e), None => Ok(self.cards) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ImageType;

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
        for k in [FrameKind::MasterLight, FrameKind::MasterDark, FrameKind::MasterBias,
                  FrameKind::MasterFlat, FrameKind::MasterDarkFlat] {
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
        assert!((ra_back - 10.684708).abs() < 0.001 / 3600.0 * 15.0, "{ra_s} -> {ra_back}");
        let dparts: Vec<f64> = dec_s[1..].split(' ').map(|p| p.parse().unwrap()).collect();
        let dec_back = dparts[0] + dparts[1] / 60.0 + dparts[2] / 3600.0;
        assert!((dec_back - 41.269065).abs() < 0.01 / 3600.0, "{dec_s} -> {dec_back}");
    }

    #[test]
    fn negative_dec_sign() {
        assert!(dec_to_sexagesimal(-16.716).starts_with('-'));
    }

    #[test]
    fn ath_keywords_are_all_within_8_chars() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ath_src("abc").ath_n(30).ath_rej("sigma3.0/2.5").ath_ver("0.2.4")
            .ath_hsh("deadbeef").ath_temp_span(-10.6, -9.8)
            .build().unwrap();
        for c in &cards {
            assert!(c.keyword.len() <= 8, "{}", c.keyword);
        }
        assert!(cards.iter().any(|c| c.keyword == "ATH_TMIN"));
    }

    #[test]
    fn builder_emits_units_in_comments() {
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .ccd_temp(-10.0).exptime(300.0).build().unwrap();
        let ccd = cards.iter().find(|c| c.keyword == "CCD-TEMP").unwrap();
        assert!(ccd.comment.as_deref().unwrap_or("").contains("[degC]"));
    }
}
