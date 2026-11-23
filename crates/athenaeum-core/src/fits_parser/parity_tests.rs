//! Parity (spec 9.1): the frame builders and the stored-text readers answer the same, field by
//! field, under the default rules and under a custom one. A stored header read back (the
//! metadata pane's snapshot, the override flag, Apply) must say what the scan stored.

use super::fits_header_reader::FitsHeader;
use super::header_keys::stored_header_key_map;
use super::{build_frame_from_header, parse_xisf_xml, xisf_header_text};
use crate::header_fallbacks::{resolve_all, HeaderField, HeaderFallbackConfig, ResolvedFields};
use crate::models::{FileFormat, Frame};
use std::path::Path;

const CARD_SIZE: usize = 80;
const BLOCK_SIZE: usize = 2880;

/// The twelve rule-driven fields of a built frame.
fn fields_of(f: &Frame) -> ResolvedFields {
    ResolvedFields {
        ccd_temp: f.ccd_temp,
        set_temp: f.set_temp,
        gain: f.gain,
        offset: f.offset,
        exptime: f.exptime,
        focallen: f.focallen,
        xpixsz: f.xpixsz,
        ypixsz: f.ypixsz,
        object: f.object.clone(),
        filter: f.filter.clone(),
        instrume: f.instrume.clone(),
        telescop: f.telescop.clone(),
    }
}

/// Default rules, and one that moves the HIERARCH keyword first and flips exposure.
fn rule_sets() -> Vec<HeaderFallbackConfig> {
    let mut custom = HeaderFallbackConfig::default();
    custom.fields.get_mut(&HeaderField::CcdTemp).unwrap().order =
        vec!["ESO DET TEMP".into(), "CCD-TEMP".into(), "DET-TEMP".into()];
    custom.fields.get_mut(&HeaderField::Exptime).unwrap().order =
        vec!["EXPOSURE".into(), "EXPTIME".into()];
    vec![HeaderFallbackConfig::default(), custom]
}

fn fits_block(cards: &[&str]) -> Vec<u8> {
    let mut block = vec![b' '; BLOCK_SIZE];
    for (i, card) in cards.iter().enumerate() {
        block[i * CARD_SIZE..i * CARD_SIZE + card.len()].copy_from_slice(card.as_bytes());
    }
    block
}

/// K as FITS cards (no duplicates: FITS keeps the first, XISF the last).
const K_CARDS: [&str; 8] = [
    "DET-TEMP=                 -5.5 / sensor",
    "HIERARCH ESO DET TEMP = -7.25 / sensor",
    "EXPOSURE=                  120 / seconds",
    "OBJECT  = 'M42     '",
    "FILTER  = 'Ha / 7nm'",
    "GAIN    =                  100",
    "PIXSIZE1=                 3.76",
    "INSTRUME= 'Cam     '",
];

fn xisf_xml(keywords: &[(&str, &str)]) -> String {
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xisf version=\"1.0\">\n<Image geometry=\"100:100:1\">\n");
    for (name, value) in keywords {
        body.push_str(&format!("<FITSKeyword name=\"{name}\" value=\"{value}\" comment=\"\"/>\n"));
    }
    body.push_str("</Image>\n</xisf>");
    body
}

/// K as XISF `FITSKeyword` elements (values in XISF quoting).
const K_XISF: [(&str, &str); 8] = [
    ("DET-TEMP", "-5.5"),
    ("ESO DET TEMP", "-7.25"),
    ("EXPOSURE", "120"),
    ("OBJECT", "'M42'"),
    ("FILTER", "'Ha / 7nm'"),
    ("GAIN", "100"),
    ("PIXSIZE1", "3.76"),
    ("INSTRUME", "'Cam'"),
];

#[test]
fn parity_fits_builder_and_stored_text_agree_under_default_and_custom_rules() {
    let mut cards: Vec<&str> = K_CARDS
        .iter()
        .copied()
        .filter(|c| !c.starts_with("OBJECT") && !c.starts_with("FILTER"))
        .collect();
    cards.push("OBJECT  = 'O''Brien'");
    cards.push("FILTER  = 'Long filter &'");
    cards.push("CONTINUE  '  two'");
    cards.push("END");
    let block = fits_block(&cards);

    for rules in rule_sets() {
        let header = FitsHeader::from_bytes(&block).unwrap();
        let frame = build_frame_from_header(&header, 0, Path::new("p.fits"), &rules).unwrap();
        let stored = resolve_all(
            &rules,
            &stored_header_key_map(FileFormat::FITS, &header.to_header_text()),
        );
        assert_eq!(fields_of(&frame), stored);
        assert_eq!(stored.object.as_deref(), Some("O'Brien"));
        assert_eq!(stored.filter.as_deref(), Some("Long filter  two"));
    }
}

#[test]
fn parity_xisf_builder_dump_and_raw_xml_agree_under_default_and_custom_rules() {
    let mut kw: Vec<(&str, &str)> = K_XISF
        .iter()
        .copied()
        .filter(|(k, _)| *k != "OBJECT")
        .collect();
    kw.push(("GAIN", "120"));
    kw.push(("OBJECT", "'  M42  '"));
    let xml = xisf_xml(&kw);
    let path = Path::new("p.xisf");
    let dump = xisf_header_text(xml.as_bytes(), path).unwrap();

    for rules in rule_sets() {
        let frame = parse_xisf_xml(xml.as_bytes(), 0, path, &rules).unwrap();
        let from_dump = resolve_all(&rules, &stored_header_key_map(FileFormat::XISF, &dump));
        let from_xml = resolve_all(&rules, &stored_header_key_map(FileFormat::XISF, &xml));
        assert_eq!(fields_of(&frame), from_dump);
        assert_eq!(from_dump, from_xml);
        assert_eq!(from_dump.filter.as_deref(), Some("Ha / 7nm"));
        assert_eq!(from_dump.gain, Some(120.0), "the last duplicate wins in XISF");
        assert_eq!(from_dump.object.as_deref(), Some("M42"));
    }
}

#[test]
fn parity_one_keyword_set_gives_the_same_fields_through_fits_and_xisf() {
    let mut cards: Vec<&str> = K_CARDS.to_vec();
    cards.push("END");
    let block = fits_block(&cards);
    let xml = xisf_xml(&K_XISF);
    let xpath = Path::new("p.xisf");
    let dump = xisf_header_text(xml.as_bytes(), xpath).unwrap();

    for rules in rule_sets() {
        let header = FitsHeader::from_bytes(&block).unwrap();
        let fits = fields_of(&build_frame_from_header(&header, 0, Path::new("p.fits"), &rules).unwrap());
        let xisf = fields_of(&parse_xisf_xml(xml.as_bytes(), 0, xpath, &rules).unwrap());
        let stored_fits = resolve_all(
            &rules,
            &stored_header_key_map(FileFormat::FITS, &header.to_header_text()),
        );
        let stored_xisf = resolve_all(&rules, &stored_header_key_map(FileFormat::XISF, &dump));
        assert_eq!(fits, xisf);
        assert_eq!(fits, stored_fits);
        assert_eq!(fits, stored_xisf);
        // The fields K carries (spec 9.1).
        assert_eq!(fits.exptime, Some(120.0));
        assert_eq!(fits.object.as_deref(), Some("M42"));
        assert_eq!(fits.xpixsz, Some(3.76));
        let hierarch_first = rules.order(HeaderField::CcdTemp)[0] == "ESO DET TEMP";
        assert_eq!(fits.ccd_temp, Some(if hierarch_first { -7.25 } else { -5.5 }));
    }
}
