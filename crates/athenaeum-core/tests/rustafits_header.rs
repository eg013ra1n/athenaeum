//! Drift guard (starloom phase 2, P2-R1): `astroimage::header` in rustafits is a port of the catalog's header reader,
//! because the headless builds of core have no rustafits. This test holds the two together: over the same files the
//! port gives the catalog's header text, the catalog's key map and the catalog's frame fields (default rules).
//!
//! Inputs: every FITS / XISF file under `tests/fixtures/`, the parser tests' own header shapes, the inputs of the
//! rule comparison of phase 2 (P2-R23, in `rustafits_header/inputs.rs`) and the real files of rustafits' own tests.
//!
//! One class is known to differ and is pinned by its own test: a `.fits` with an empty primary and the image in an
//! extension HDU. rustafits reads its logical header (the header the pixel reader decodes); the catalog stores the
//! primary header.
#![cfg(feature = "render")]

#[path = "rustafits_header/inputs.rs"]
mod inputs;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use astroimage::header::{self, FrameFacts, HeaderFormat};
use athenaeum_core::calibration::geometry::origin_from_keys;
use athenaeum_core::fits_parser::header_keys::stored_header_key_map;
use athenaeum_core::fits_parser::stored_header::parse_stored_header_keys;
use athenaeum_core::fits_parser::{extract_fits_header, extract_xisf_header, parse_date_obs, parse_fits_with_header, parse_xisf};
use athenaeum_core::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue};
use athenaeum_core::header_fallbacks::HeaderFallbackConfig;
use athenaeum_core::models::{FileFormat, Frame};

const BLOCK: usize = 2880;

/// FITS header block(s): the cards, then `END`, blank padded to whole blocks.
fn fits_header_blocks(cards: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for card in cards.iter().copied().chain(std::iter::once("END")) {
        assert!(card.len() <= 80, "a card is at most 80 bytes: {card}");
        let mut padded = [b' '; 80];
        padded[..card.len()].copy_from_slice(card.as_bytes());
        bytes.extend_from_slice(&padded);
    }
    while bytes.len() % BLOCK != 0 {
        bytes.push(b' ');
    }
    bytes
}

/// A FITS file: the header and one block of zero data.
fn write_fits(path: &Path, cards: &[&str]) {
    let mut bytes = fits_header_blocks(cards);
    bytes.extend_from_slice(&[0u8; BLOCK]);
    std::fs::write(path, bytes).unwrap();
}

/// An XISF file: the 16-byte prelude, the XML, `attachment` zero bytes.
fn write_xisf(path: &Path, xml: &str, attachment: usize) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"XISF0100");
    bytes.extend_from_slice(&(xml.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0u8; 4]);
    bytes.extend_from_slice(xml.as_bytes());
    bytes.extend_from_slice(&vec![0u8; attachment]);
    std::fs::write(path, bytes).unwrap();
}

/// The XML of a minimal XISF header the way the parser tests write it: `body` inside one `<Image>`.
fn xisf_xml(geometry: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xisf version=\"1.0\">\n<Image geometry=\"{geometry}\">\n{body}</Image>\n</xisf>"
    )
}

fn keyword(name: &str, value: &str) -> String {
    format!("<FITSKeyword name=\"{name}\" value=\"{value}\" comment=\"\"/>\n")
}

/// What the catalog says about a file: its frame, its stored header text and the key map of that text.
struct CatalogView {
    format: FileFormat,
    frame: Frame,
    text: String,
    keys: HashMap<String, String>,
}

fn is_xisf(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("xisf"))
}

fn catalog_view(path: &Path) -> Result<CatalogView, String> {
    let rules = HeaderFallbackConfig::default();
    if is_xisf(path) {
        let frame = parse_xisf(path, 1, &rules).map_err(|e| format!("catalog parse_xisf: {e:#}"))?;
        let text = extract_xisf_header(path).map_err(|e| format!("catalog extract_xisf_header: {e:#}"))?;
        let keys = parse_stored_header_keys(FileFormat::XISF, &text);
        Ok(CatalogView { format: FileFormat::XISF, frame, text, keys })
    } else {
        let (frame, text) = parse_fits_with_header(path, 1, &rules).map_err(|e| format!("catalog parse_fits: {e:#}"))?;
        let text_alone = extract_fits_header(path).map_err(|e| format!("catalog extract_fits_header: {e:#}"))?;
        if text != text_alone {
            return Err("catalog: parse_fits_with_header and extract_fits_header give different text".to_string());
        }
        let keys = parse_stored_header_keys(FileFormat::FITS, &text);
        Ok(CatalogView { format: FileFormat::FITS, frame, text, keys })
    }
}

fn bits(v: Option<f64>) -> Option<u64> {
    v.map(f64::to_bits)
}

/// Collects every difference of one file, so a failing run names all of them.
struct Diffs {
    file: String,
    lines: Vec<String>,
}

impl Diffs {
    fn new(path: &Path) -> Self {
        Self { file: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), lines: Vec::new() }
    }

    fn push(&mut self, what: &str, catalog: &dyn std::fmt::Debug, rustafits: &dyn std::fmt::Debug) {
        self.lines.push(format!("{}: {what}: catalog {catalog:?}, rustafits {rustafits:?}", self.file));
    }

    fn same<T: PartialEq + std::fmt::Debug>(&mut self, what: &str, catalog: T, rustafits: T) {
        if catalog != rustafits {
            self.push(what, &catalog, &rustafits);
        }
    }

    fn same_f64(&mut self, what: &str, catalog: Option<f64>, rustafits: Option<f64>) {
        if bits(catalog) != bits(rustafits) {
            self.push(what, &catalog, &rustafits);
        }
    }
}

/// The catalog's and rustafits' reading of one file, compared. `Ok(diffs)`; `Err` when the two disagree on whether
/// the file can be read at all, or both fail (a corpus input that neither reads is a fault of the corpus).
fn compare_file(path: &Path) -> Vec<String> {
    let mut d = Diffs::new(path);
    let catalog = catalog_view(path);
    let text = header::header_text(path);
    let facts = header::read_frame_facts(path);
    let (catalog, text, facts) = match (catalog, text, facts) {
        (Ok(c), Ok(t), Ok(f)) => (c, t, f),
        (c, t, f) => {
            d.lines.push(format!(
                "{}: a reader failed: catalog {:?}, header_text {:?}, read_frame_facts {:?}",
                d.file,
                c.as_ref().err(),
                t.as_ref().err().map(|e| format!("{e:#}")),
                f.as_ref().err().map(|e| format!("{e:#}")),
            ));
            return d.lines;
        }
    };
    let (rf_format, rf_text) = text;

    // 1. The header text.
    d.same("header format", catalog.format == FileFormat::XISF, rf_format == HeaderFormat::Xisf);
    d.same("header text", &catalog.text, &rf_text);

    // 2. The key maps of the stored text.
    let core_map = stored_header_key_map(catalog.format.clone(), &catalog.text);
    let rf_map = header::header_key_map(rf_format, &rf_text);
    let rf_legacy = header::header_key_map(rf_format, &rf_text).into_legacy_map();
    d.same("legacy key map", &catalog.keys, &rf_legacy);
    let mut probe: BTreeSet<String> = catalog.keys.keys().cloned().chain(rf_legacy.keys().cloned()).collect();
    for extra in ["ccd-temp", "Exptime", "HIERARCH ESO DET TEMP", "XBAYROFF", "ybayroff", "NOT-A-KEY", ""] {
        probe.insert(extra.to_string());
    }
    for key in &probe {
        d.same(&format!("key map answer for {key:?}"), core_map.get(key), rf_map.get(key));
    }

    // 3. The frame fields under the default rules.
    let f: &Frame = &catalog.frame;
    let r: &FrameFacts = &facts;
    d.same("facts format", catalog.format == FileFormat::XISF, r.format == HeaderFormat::Xisf);
    d.same("naxis1", f.naxis1.map(i64::from), r.width);
    d.same("naxis2", f.naxis2.map(i64::from), r.height);
    d.same("imagetyp", f.imagetyp.as_ref().map(|t| format!("{t:?}")), r.imagetyp.clone());
    d.same_f64("exptime", f.exptime, r.exptime_s);
    d.same_f64("gain", f.gain, r.gain);
    d.same_f64("offset", f.offset, r.offset);
    d.same_f64("ccd_temp", f.ccd_temp, r.ccd_temp);
    d.same_f64("set_temp", f.set_temp, r.set_temp);
    d.same("filter", f.filter.clone(), r.filter.clone());
    d.same("object", f.object.clone(), r.object.clone());
    d.same("instrume", f.instrume.clone(), r.instrume.clone());
    d.same("telescop", f.telescop.clone(), r.telescop.clone());
    d.same_f64("focallen", f.focallen, r.focallen_mm);
    d.same_f64("xpixsz", f.xpixsz, r.xpixsz_um);
    d.same_f64("ypixsz", f.ypixsz, r.ypixsz_um);
    d.same("xbinning", f.xbinning.map(i64::from), r.xbinning);
    d.same("ybinning", f.ybinning.map(i64::from), r.ybinning);
    // The date is raw text in rustafits: the catalog's parse of it is the catalog's value.
    let parsed = r.date_obs.as_deref().and_then(|date| parse_date_obs(date, r.time_obs.as_deref()).ok());
    d.same("date_obs", f.date_obs, parsed);
    d.same("bayerpat", f.bayerpat.clone(), r.bayerpat.clone());
    // The Bayer offsets are the catalog's columns (`frames.xbayroff`), the name as written.
    d.same("xbayroff", f.xbayroff, r.xbayroff);
    d.same("ybayroff", f.ybayroff, r.ybayroff);
    d.same("roworder", f.roworder.clone(), r.roworder.clone());
    d.same(
        "origin",
        origin_from_keys(&catalog.keys).map(|(x, y)| (i64::from(x), i64::from(y))),
        r.origin,
    );
    d.same("calstat", catalog.keys.get("CALSTAT").cloned(), r.calstat.clone());
    d.lines
}

fn report(what: &str, checked: usize, lines: Vec<String>) {
    assert!(checked > 0, "{what}: no input was checked");
    assert!(lines.is_empty(), "{what}: {} difference(s) over {checked} input(s):\n{}", lines.len(), lines.join("\n"));
}

/// Every `.fits` / `.fit` / `.fts` / `.xisf` file under `dir`, sorted.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())).map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["fits", "fit", "fts", "xisf"].iter().any(|k| e.eq_ignore_ascii_case(k)))
        {
            out.push(path);
        }
    }
}

/// The parser tests' own header shapes, written to `dir` (the shapes of `fits_parser/mod.rs` and `parity_tests.rs`).
fn parser_test_inputs(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut fits = |name: &str, cards: &[&str]| {
        let path = dir.join(name);
        write_fits(&path, cards);
        paths.push(path);
    };
    // `write_light_fits`: a real-looking light header.
    let light = [
        "SIMPLE  =                    T",
        "BITPIX  =                   16",
        "NAXIS   =                    2",
        "NAXIS1  =                    4",
        "NAXIS2  =                    2",
        "OBJECT  = 'M33     '",
        "TELESCOP= 'RC8     '",
        "INSTRUME= 'ASI2600MM'",
        "IMAGETYP= 'Light   '",
        "FILTER  = 'Ha      '",
        "EXPTIME =                300.0",
        "GAIN    =                  100",
        "OFFSET  =                   50",
        "XBINNING=                    1",
        "YBINNING=                    1",
        "CCD-TEMP=                -10.0",
        "DATE-OBS= '2024-01-15T20:30:45.123'",
        "RA      =              23.4621",
        "DEC     =              30.6599",
    ];
    fits("light.fits", &light);
    // The rule-set cards K of the parity tests, with a doubled quote and a CONTINUE card.
    fits(
        "parity-k.fits",
        &[
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    2",
            "DET-TEMP=                 -5.5 / sensor",
            "HIERARCH ESO DET TEMP = -7.25 / sensor",
            "EXPOSURE=                  120 / seconds",
            "GAIN    =                  100",
            "PIXSIZE1=                 3.76",
            "INSTRUME= 'Cam     '",
            "OBJECT  = 'O''Brien'",
            "FILTER  = 'Long filter &'",
            "CONTINUE  '  two'",
        ],
    );
    // The Bayer tests: offsets, the quoted and the float spelling, a row order.
    fits(
        "bayer.fits",
        &[
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    2",
            "BAYERPAT= 'RGGB    '",
            "XBAYROFF=                    1",
            "YBAYROFF=                  1.0",
            "ROWORDER= 'TOP-DOWN'",
        ],
    );
    // A calibrated light's cards and a sub-frame origin.
    fits(
        "calibrated.fits",
        &[
            "SIMPLE  =                    T",
            "BITPIX  =                  -32",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    2",
            "IMAGETYP= 'Master Dark'",
            "CALSTAT = 'BDF     '",
            "XORGSUBF=                 1584",
            "YORGSUBF=                1064.",
            "FRAME   = 'Flat    '",
        ],
    );
    // No IMAGETYP, a FRAME card; a date and a time of its own.
    fits(
        "frame-and-time.fits",
        &[
            "SIMPLE  =                    T",
            "BITPIX  =                   16",
            "NAXIS   =                    2",
            "NAXIS1  =                    4",
            "NAXIS2  =                    2",
            "FRAME   = 'Dome Flat'",
            "DATE-OBS= '2024-01-15'",
            "TIME-OBS= '20:30:45'",
            "CCD-TEMP= 'warm'",
            "DET-TEMP=                -12.5",
        ],
    );

    let mut xisf = |name: &str, xml: String| {
        let path = dir.join(name);
        write_xisf(&path, &xml, 16);
        paths.push(path);
    };
    // `write_xisf_with_keywords`: quoted values, the Bayer keywords.
    let mut body = String::new();
    for (k, v) in [("BAYERPAT", "'RGGB'"), ("XBAYROFF", "1"), ("YBAYROFF", "0"), ("ROWORDER", "'TOP-DOWN'")] {
        body.push_str(&keyword(k, v));
    }
    xisf("osc.xisf", xisf_xml("100:100:1", &body));
    xisf("no-naxis.xisf", xisf_xml("100:100:1", &keyword("IMAGETYP", "'Dark'")));
    xisf("absent-offsets.xisf", xisf_xml("100:100:1", &keyword("BAYERPAT", "'RGGB'")));
    // The parity tests' XISF set: a repeated GAIN (the last wins), padded OBJECT, a slash in a value.
    let mut body = String::new();
    for (k, v) in [
        ("DET-TEMP", "-5.5"),
        ("ESO DET TEMP", "-7.25"),
        ("EXPOSURE", "120"),
        ("FILTER", "'Ha / 7nm'"),
        ("GAIN", "100"),
        ("PIXSIZE1", "3.76"),
        ("INSTRUME", "'Cam'"),
        ("GAIN", "120"),
        ("OBJECT", "'  M42  '"),
    ] {
        body.push_str(&keyword(k, v));
    }
    xisf("parity-k.xisf", xisf_xml("100:100:1", &body));
    // `<ColorFilterArray>`, alone, with a blank BAYERPAT keyword, with a disagreeing keyword.
    let cfa = "<ColorFilterArray pattern=\"RGGB\" width=\"2\" height=\"2\" name=\"RGGB\"/>\n";
    xisf("cfa-only.xisf", xisf_xml("100:100:1", &format!("{}{cfa}", keyword("OBJECT", "'M42'"))));
    xisf("cfa-keyword-wins.xisf", xisf_xml("100:100:1", &format!("{}{cfa}", keyword("BAYERPAT", "'BGGR'"))));
    xisf("cfa-blank-keyword.xisf", xisf_xml("100:100:1", &format!("{}{cfa}", keyword("BAYERPAT", "''"))));
    xisf(
        "cfa-unrecognized.xisf",
        xisf_xml("100:100:1", "<ColorFilterArray pattern=\"XYZW\" width=\"2\" height=\"2\"/>\n"),
    );
    // The pixel-size and binning `<Property>` elements, after the keywords.
    let props = "<Property id=\"Instrument:Sensor:XPixelSize\" type=\"Float\" value=\"3.76\"/>\n\
                 <Property id=\"Instrument:Sensor:YPixelSize\" type=\"Float\" value=\"3.77\"/>\n\
                 <Property id=\"Instrument:Camera:XBinning\" type=\"Int32\" value=\"2\"/>\n\
                 <Property id=\"Instrument:Camera:YBinning\" type=\"Int32\" value=\"2\"/>\n";
    xisf("properties.xisf", xisf_xml("100:100:1", props));
    xisf("properties-and-keyword.xisf", xisf_xml("100:100:1", &format!("{props}{}", keyword("PIXSIZE1", "4.63"))));
    // The writer's own output, read back (a master).
    let writer_cards = vec![
        Card::new("OBJECT", CardValue::Str("M33".into())).unwrap(),
        Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
        Card::new("FILTER", CardValue::Str("OIII".into())).unwrap(),
        Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
        Card::new("GAIN", CardValue::Real(100.0)).unwrap(),
        Card::new("XBINNING", CardValue::Integer(1)).unwrap(),
        Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
    ];
    let path = dir.join("writer.xisf");
    write_xisf_f32(&path, 4, 2, 1, &[0.1; 8], &writer_cards).unwrap();
    paths.push(path);
    let path = dir.join("writer.fits");
    write_fits_f32(&path, 4, 2, 1, &[0.1; 8], &writer_cards).unwrap();
    paths.push(path);
    paths
}

/// The inputs of P2-R23, written to `dir`.
fn rule_comparison_inputs(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (name, cards) in inputs::FITS_INPUTS {
        let path = dir.join(format!("{name}.fits"));
        write_fits(&path, cards);
        paths.push(path);
    }
    for (name, xml, attachment) in inputs::XISF_INPUTS {
        let path = dir.join(format!("{name}.xisf"));
        write_xisf(&path, xml, *attachment);
        paths.push(path);
    }
    paths
}

#[test]
fn the_rustafits_header_reader_agrees_with_the_catalog_reader() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let mut files = Vec::new();
    collect_files(&fixtures, &mut files);
    assert!(!files.is_empty(), "no FITS or XISF fixture under {}", fixtures.display());
    let fixture_count = files.len();

    let dir = tempfile::TempDir::new().unwrap();
    let parser_dir = dir.path().join("parser");
    let rules_dir = dir.path().join("rules");
    std::fs::create_dir_all(&parser_dir).unwrap();
    std::fs::create_dir_all(&rules_dir).unwrap();
    let parser = parser_test_inputs(&parser_dir);
    let rules = rule_comparison_inputs(&rules_dir);
    assert_eq!(rules.len(), inputs::FITS_INPUTS.len() + inputs::XISF_INPUTS.len());
    files.extend(parser.iter().cloned());
    files.extend(rules.iter().cloned());

    let mut lines = Vec::new();
    for path in &files {
        lines.extend(compare_file(path));
    }
    eprintln!(
        "rustafits_header drift guard: {} files ({fixture_count} fixtures, {} parser inputs, {} rule inputs)",
        files.len(),
        parser.len(),
        rules.len()
    );
    report("catalog reader vs astroimage::header", files.len(), lines);
}

/// Real files from rustafits' own tests (not an extension-HDU or compressed file: those are the pinned class).
#[test]
fn the_rustafits_header_reader_agrees_with_the_catalog_reader_on_real_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("rustafits").join("tests");
    assert!(root.is_dir(), "the rustafits submodule is not checked out: {}", root.display());
    let mut files = Vec::new();
    collect_files(&root, &mut files);
    // The tile-compressed files live in `fpack`: the pinned class, read by their logical header.
    files.retain(|p| !p.components().any(|c| c.as_os_str() == "fpack"));
    assert!(!files.is_empty(), "no real file under {}", root.display());
    let mut lines = Vec::new();
    for path in &files {
        lines.extend(compare_file(path));
    }
    eprintln!("rustafits_header real files: {}", files.len());
    report("real files", files.len(), lines);
}

/// The one known class: an empty primary and the image in an extension HDU. rustafits reads the logical header (the
/// image's own), the catalog the primary one. The difference is pinned so that it cannot change silently.
#[test]
fn an_extension_hdu_fits_is_read_by_its_logical_header_in_rustafits_and_by_the_primary_in_the_catalog() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("extension.fits");
    let mut bytes = fits_header_blocks(&[
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "EXTEND  =                    T",
        "OBJECT  = 'PRIMARY '",
    ]);
    bytes.extend_from_slice(&fits_header_blocks(&[
        "XTENSION= 'IMAGE   '",
        "BITPIX  =                   16",
        "NAXIS   =                    2",
        "NAXIS1  =                    4",
        "NAXIS2  =                    2",
        "PCOUNT  =                    0",
        "GCOUNT  =                    1",
        "OBJECT  = 'EXTENSION'",
        "EXPTIME =                300.0",
    ]));
    bytes.extend_from_slice(&[0u8; BLOCK]);
    std::fs::write(&path, bytes).unwrap();

    let catalog = catalog_view(&path).unwrap();
    assert_eq!(catalog.frame.object.as_deref(), Some("PRIMARY"), "the catalog reads the primary header");
    assert_eq!(catalog.frame.exptime, None);
    assert_eq!((catalog.frame.naxis1, catalog.frame.naxis2), (None, None));

    let (format, text) = header::header_text(&path).unwrap();
    assert_eq!(format, HeaderFormat::Fits);
    assert_ne!(text, catalog.text, "the logical header is not the primary header");
    assert!(text.contains("EXTENSION"), "rustafits' header text is the image's: {text}");

    let facts = header::read_frame_facts(&path).unwrap();
    assert_eq!(facts.object.as_deref(), Some("EXTENSION"));
    assert_eq!(facts.exptime_s, Some(300.0));
    assert_eq!((facts.width, facts.height), (Some(4), Some(2)));
}
