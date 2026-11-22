use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use anyhow::{Context, Result};

const CARD_SIZE: usize = 80;
const BLOCK_SIZE: usize = 2880;

/// Parsed FITS primary HDU header.
pub struct FitsHeader {
    /// Keyword -> value (first occurrence wins for duplicates, except COMMENT/HISTORY)
    keywords: HashMap<String, String>,
    /// Keywords in the order each was first inserted (HIERARCH keys as written).
    order: Vec<String>,
    /// Raw 80-byte card images in order (for extract_fits_header)
    raw_cards: Vec<String>,
}

/// The card-by-card state machine shared by the block reader (a scan) and the stored-text
/// reader (`FitsHeader::from_header_text`), so both produce the same values from the same cards.
#[derive(Default)]
struct CardAccumulator {
    keywords: HashMap<String, String>,
    order: Vec<String>,
    raw_cards: Vec<String>,
    last_keyword: Option<String>,
    last_had_continuation: bool,
}

impl CardAccumulator {
    /// First occurrence wins; the key joins `order` when first inserted.
    fn insert_first(&mut self, key: String, value: String) {
        if let std::collections::hash_map::Entry::Vacant(e) = self.keywords.entry(key) {
            self.order.push(e.key().clone());
            e.insert(value);
        }
    }

    /// Feed one card image. Returns true once the END card has been consumed.
    fn push_card(&mut self, card: &str) -> bool {
        self.raw_cards.push(card.to_string());

        let keyword = card.get(0..8).unwrap_or("").trim().to_uppercase();

        if keyword == "END" {
            return true;
        }

        if keyword == "CONTINUE" {
            if self.last_had_continuation {
                if let Some(key) = self.last_keyword.clone() {
                    let value_area = card.get(8..).unwrap_or("").trim();
                    let (continued_value, has_amp) = parse_quoted_string(value_area);
                    if let Some(existing) = self.keywords.get_mut(&key) {
                        existing.push_str(&continued_value);
                    }
                    self.last_had_continuation = has_amp;
                }
            }
            return false;
        }

        if keyword == "COMMENT" || keyword == "HISTORY" || keyword.is_empty() {
            let text = card.get(8..).unwrap_or("").trim_end().to_string();
            match self.keywords.get_mut(&keyword) {
                Some(v) => {
                    v.push('\n');
                    v.push_str(&text);
                }
                None => {
                    self.order.push(keyword.clone());
                    self.keywords.insert(keyword, text);
                }
            }
            self.last_keyword = None;
            self.last_had_continuation = false;
            return false;
        }

        if keyword == "HIERARCH" {
            if let Some((key, value)) = parse_hierarch_card(card) {
                self.last_keyword = Some(key.clone());
                self.last_had_continuation = false;
                self.insert_first(key, value);
                return false;
            }
        }

        let indicator = card.get(8..10).unwrap_or("");
        if indicator != "= " {
            self.last_keyword = None;
            self.last_had_continuation = false;
            return false;
        }

        let value_area = card.get(10..).unwrap_or("").trim_start();

        if value_area.starts_with('\'') {
            let (value, has_amp) = parse_quoted_string(value_area);
            self.last_keyword = Some(keyword.clone());
            self.last_had_continuation = has_amp;
            self.insert_first(keyword, value);
        } else {
            let value = extract_value_before_comment(value_area);
            self.last_keyword = Some(keyword.clone());
            self.last_had_continuation = false;
            self.insert_first(keyword, value);
        }
        false
    }

    fn finish(self) -> FitsHeader {
        FitsHeader { keywords: self.keywords, order: self.order, raw_cards: self.raw_cards }
    }
}

impl FitsHeader {
    /// Read primary HDU header from a FITS file.
    /// Only reads the header blocks (typically 3-30KB), not the entire file.
    pub fn from_path(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open FITS file: {}", path.display()))?;
        let mut reader = std::io::BufReader::new(file);
        Self::parse_from_reader(&mut reader)
    }

    /// Parse header by reading one 2880-byte block at a time from a reader.
    /// Stops as soon as the END card is found — typically reads 1-10 blocks
    /// (2.8KB - 28KB) instead of the entire file (20-200MB+).
    fn parse_from_reader(reader: &mut impl Read) -> Result<Self> {
        let mut acc = CardAccumulator::default();
        let mut buf = [0u8; BLOCK_SIZE];

        loop {
            let bytes_read = read_exact_or_eof(reader, &mut buf)?;
            if bytes_read == 0 {
                anyhow::bail!("FITS header not terminated with END card");
            }

            let block = &buf[..bytes_read];

            // Process each 80-byte card in this block
            let mut card_offset = 0;
            while card_offset + CARD_SIZE <= block.len() {
                let card_bytes = &block[card_offset..card_offset + CARD_SIZE];
                let card_str = String::from_utf8_lossy(card_bytes);
                if acc.push_card(card_str.as_ref()) {
                    return Ok(acc.finish());
                }
                card_offset += CARD_SIZE;
            }
        }
    }

    /// Rebuild a header from the text `to_header_text` stored (one card per line). Same card
    /// state machine as a scan, so the stored text reads exactly as the scan read the file.
    /// A trailing `\r` is stripped, a line shorter than 80 bytes is padded (persistence may
    /// have trimmed it), reading stops at END, and nothing here can fail.
    pub(crate) fn from_header_text(text: &str) -> Self {
        let mut acc = CardAccumulator::default();
        for line in text.lines() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let done = if line.len() < CARD_SIZE {
                let mut padded = String::with_capacity(CARD_SIZE);
                padded.push_str(line);
                while padded.len() < CARD_SIZE {
                    padded.push(' ');
                }
                acc.push_card(&padded)
            } else {
                acc.push_card(line)
            };
            if done {
                break;
            }
        }
        acc.finish()
    }

    /// Every keyword with its raw value, in the order each was first inserted (HIERARCH keys
    /// as written; COMMENT / HISTORY / blank-keyword entries included).
    pub(crate) fn keywords_in_order(&self) -> impl Iterator<Item = (&str, &str)> {
        self.order
            .iter()
            .filter_map(move |k| self.keywords.get(k).map(|v| (k.as_str(), v.as_str())))
    }

    /// Parse header from raw bytes: the header blocks of a FITS file through the END card
    /// (the zip header pass hands over exactly that). Same parser as `from_path`.
    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut cursor = std::io::Cursor::new(data);
        Self::parse_from_reader(&mut cursor)
    }

    /// Test alias of `from_bytes`.
    #[cfg(test)]
    pub(crate) fn parse_header(data: &[u8]) -> Result<Self> {
        Self::from_bytes(data)
    }

    /// Get keyword value as a trimmed string.
    pub fn get_str(&self, key: &str) -> Option<String> {
        self.keywords.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    }

    /// Get keyword value as f64, handling Fortran D-notation.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.keywords.get(key).and_then(|v| parse_fits_f64(v.trim()))
    }

    /// Get keyword value as i32.
    pub fn get_i32(&self, key: &str) -> Option<i32> {
        self.keywords.get(key).and_then(|v| {
            let s = v.trim();
            // Try i64 first (handles large values), then truncate to i32
            s.parse::<i64>().ok().map(|n| n as i32)
                .or_else(|| parse_fits_f64(s).map(|f| f as i32))
        })
    }

    /// Format the raw header cards as text for DB storage.
    pub fn to_header_text(&self) -> String {
        self.raw_cards.join("\n")
    }
}

/// Read exactly `buf.len()` bytes, or fewer if EOF is reached.
/// Returns the number of bytes actually read.
fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break, // EOF
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}

/// Parse a quoted string value from FITS card value area.
/// Returns (value, has_continuation_marker).
fn parse_quoted_string(s: &str) -> (String, bool) {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes[0] != b'\'' {
        return (String::new(), false);
    }

    let mut result = String::new();
    let mut i = 1; // skip opening quote

    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // Check for escaped quote ''
            if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                result.push('\'');
                i += 2;
            } else {
                // Closing quote
                break;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    // Check for continuation marker '&' at end (before trimming)
    let has_continuation = result.ends_with('&');
    if has_continuation {
        result.pop(); // strip '&'
    }

    // Trim trailing spaces (FITS convention)
    let trimmed = result.trim_end().to_string();
    (trimmed, has_continuation)
}

/// Extract value portion before the comment delimiter '/'.
/// Only considers '/' outside of quoted strings.
fn extract_value_before_comment(s: &str) -> String {
    // For non-string values, find the first '/'
    if let Some(pos) = s.find('/') {
        s[..pos].trim().to_string()
    } else {
        s.trim().to_string()
    }
}

/// Parse a HIERARCH card: "HIERARCH long.key.name = value / comment"
fn parse_hierarch_card(card: &str) -> Option<(String, String)> {
    // Find " = " after "HIERARCH "
    let after_hierarch = card.get(9..)?; // skip "HIERARCH "
    let eq_pos = after_hierarch.find(" = ")?;
    let key = after_hierarch[..eq_pos].trim().to_string();
    let value_area = after_hierarch[eq_pos + 3..].trim_start();

    if value_area.starts_with('\'') {
        let (value, _) = parse_quoted_string(value_area);
        Some((key, value))
    } else {
        let value = extract_value_before_comment(value_area);
        Some((key, value))
    }
}

/// Parse a FITS float value, handling Fortran D-notation (e.g., "1.23D+05").
pub(crate) fn parse_fits_f64(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    // Replace Fortran D/d exponent notation with E/e
    let normalized = s.replace('D', "E").replace('d', "e");
    normalized.trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quoted_string_simple() {
        let (val, cont) = parse_quoted_string("'NGC 1234  '");
        assert_eq!(val, "NGC 1234");
        assert!(!cont);
    }

    #[test]
    fn test_parse_quoted_string_escaped_quote() {
        let (val, cont) = parse_quoted_string("'O''Brien '");
        assert_eq!(val, "O'Brien");
        assert!(!cont);
    }

    #[test]
    fn test_parse_quoted_string_continuation() {
        let (val, cont) = parse_quoted_string("'This is a long string &'");
        assert_eq!(val, "This is a long string");
        assert!(cont);
    }

    #[test]
    fn test_parse_quoted_string_with_slash() {
        let (val, _) = parse_quoted_string("'R/clear '");
        assert_eq!(val, "R/clear");
    }

    #[test]
    fn test_parse_fits_f64_standard() {
        assert_eq!(parse_fits_f64("123.456"), Some(123.456));
        assert_eq!(parse_fits_f64("-45.678"), Some(-45.678));
    }

    #[test]
    fn test_parse_fits_f64_d_notation() {
        assert_eq!(parse_fits_f64("1.23D+05"), Some(1.23e5));
        assert_eq!(parse_fits_f64("1.23d-03"), Some(1.23e-3));
    }

    #[test]
    fn test_parse_fits_f64_e_notation() {
        assert_eq!(parse_fits_f64("1.23E+05"), Some(1.23e5));
    }

    #[test]
    fn test_parse_fits_f64_empty() {
        assert_eq!(parse_fits_f64(""), None);
    }

    #[test]
    fn test_extract_value_before_comment() {
        assert_eq!(extract_value_before_comment("123.456 / some comment"), "123.456");
        assert_eq!(extract_value_before_comment("  42  "), "42");
    }

    #[test]
    fn test_get_i32_from_float() {
        let mut keywords = HashMap::new();
        keywords.insert("NAXIS1".to_string(), "4144.0".to_string());
        let header = FitsHeader { keywords, order: vec![], raw_cards: vec![] };
        assert_eq!(header.get_i32("NAXIS1"), Some(4144));
    }

    #[test]
    fn test_parse_minimal_fits_header() {
        // Build a minimal FITS header (one 2880-byte block)
        let mut block = vec![b' '; BLOCK_SIZE];

        fn write_card(block: &mut [u8], index: usize, card: &str) {
            let start = index * CARD_SIZE;
            let bytes = card.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if start + i < block.len() {
                    block[start + i] = b;
                }
            }
        }

        write_card(&mut block, 0, "SIMPLE  =                    T / Standard FITS");
        write_card(&mut block, 1, "BITPIX  =                   16 / bits per pixel");
        write_card(&mut block, 2, "NAXIS   =                    2 / number of axes");
        write_card(&mut block, 3, "OBJECT  = 'M 42    '           / Target name");
        write_card(&mut block, 4, "EXPTIME =              120.000 / Exposure time");
        write_card(&mut block, 5, "FILTER  = 'R/clear '           / Filter name");
        write_card(&mut block, 6, "END                                                                             ");

        let header = FitsHeader::parse_header(&block).unwrap();
        assert_eq!(header.get_str("OBJECT"), Some("M 42".to_string()));
        assert_eq!(header.get_f64("EXPTIME"), Some(120.0));
        assert_eq!(header.get_i32("BITPIX"), Some(16));
        assert_eq!(header.get_i32("NAXIS"), Some(2));
        assert_eq!(header.get_str("FILTER"), Some("R/clear".to_string()));
    }

    #[test]
    fn test_hierarch_keyword() {
        let mut block = vec![b' '; BLOCK_SIZE];

        fn write_card(block: &mut [u8], index: usize, card: &str) {
            let start = index * CARD_SIZE;
            let bytes = card.as_bytes();
            for (i, &b) in bytes.iter().enumerate() {
                if start + i < block.len() {
                    block[start + i] = b;
                }
            }
        }

        write_card(&mut block, 0, "SIMPLE  =                    T / Standard FITS");
        write_card(&mut block, 1, "HIERARCH ESO.TEL.FWHM = 0.85 / Seeing FWHM                                    ");
        write_card(&mut block, 2, "END                                                                             ");

        let header = FitsHeader::parse_header(&block).unwrap();
        assert_eq!(header.get_f64("ESO.TEL.FWHM"), Some(0.85));
    }

    /// Lay `cards` out as 80-byte cards in one padded 2880-byte block.
    fn block_of(cards: &[&str]) -> Vec<u8> {
        let mut block = vec![b' '; BLOCK_SIZE];
        for (i, card) in cards.iter().enumerate() {
            block[i * CARD_SIZE..i * CARD_SIZE + card.len()].copy_from_slice(card.as_bytes());
        }
        block
    }

    #[test]
    fn from_header_text_matches_from_bytes_for_the_same_cards() {
        let block = block_of(&[
            "SIMPLE  =                    T",
            "HIERARCH ESO DET TEMP = -7.25 / c",
            "OBJECT  = 'O''Brien'",
            "FILTER  = 'Long filter &'",
            "CONTINUE  '  two'",
            "GAIN    = 100",
            "GAIN    = 120",
            "COMMENT first",
            "COMMENT second",
            "END",
        ]);
        let a = FitsHeader::from_bytes(&block).unwrap();
        let b = FitsHeader::from_header_text(&a.to_header_text());
        assert_eq!(
            a.keywords_in_order().collect::<Vec<_>>(),
            b.keywords_in_order().collect::<Vec<_>>()
        );
        assert_eq!(b.get_str("OBJECT").as_deref(), Some("O'Brien"));
        assert_eq!(b.get_str("FILTER").as_deref(), Some("Long filter  two"));
        assert_eq!(b.get_f64("GAIN"), Some(100.0), "first duplicate wins");
        assert_eq!(b.get_f64("ESO DET TEMP"), Some(-7.25));
    }

    #[test]
    fn from_header_text_pads_short_lines_and_strips_crlf() {
        let h = FitsHeader::from_header_text("EXPTIME = 300.0\r\nEND");
        assert_eq!(h.get_f64("EXPTIME"), Some(300.0));
    }

    #[test]
    fn from_header_text_stops_at_end_and_never_fails() {
        let h = FitsHeader::from_header_text("GAIN    = 1\nEND\nGAIN2   = 2\n");
        assert_eq!(h.get_f64("GAIN"), Some(1.0));
        assert_eq!(h.get_f64("GAIN2"), None);
        let empty = FitsHeader::from_header_text("");
        assert_eq!(empty.keywords_in_order().count(), 0);
    }
}
