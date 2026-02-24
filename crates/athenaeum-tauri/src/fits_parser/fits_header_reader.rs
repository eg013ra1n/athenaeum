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
    /// Raw 80-byte card images in order (for extract_fits_header)
    raw_cards: Vec<String>,
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
        let mut keywords: HashMap<String, String> = HashMap::new();
        let mut raw_cards: Vec<String> = Vec::new();
        let mut last_keyword: Option<String> = None;
        let mut last_had_continuation = false;
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
                let card = card_str.as_ref();

                raw_cards.push(card.to_string());

                let keyword = card.get(0..8).unwrap_or("").trim().to_uppercase();

                if keyword == "END" {
                    return Ok(FitsHeader { keywords, raw_cards });
                }

                if keyword == "CONTINUE" {
                    if last_had_continuation {
                        if let Some(ref key) = last_keyword {
                            let value_area = card.get(8..).unwrap_or("").trim();
                            let (continued_value, has_amp) = parse_quoted_string(value_area);
                            if let Some(existing) = keywords.get_mut(key) {
                                existing.push_str(&continued_value);
                            }
                            last_had_continuation = has_amp;
                        }
                    }
                    card_offset += CARD_SIZE;
                    continue;
                }

                if keyword == "COMMENT" || keyword == "HISTORY" || keyword.is_empty() {
                    let text = card.get(8..).unwrap_or("").trim_end().to_string();
                    keywords
                        .entry(keyword.clone())
                        .and_modify(|v| {
                            v.push('\n');
                            v.push_str(&text);
                        })
                        .or_insert(text);
                    last_keyword = None;
                    last_had_continuation = false;
                    card_offset += CARD_SIZE;
                    continue;
                }

                if keyword == "HIERARCH" {
                    if let Some((key, value)) = parse_hierarch_card(card) {
                        last_keyword = Some(key.clone());
                        last_had_continuation = false;
                        keywords.entry(key).or_insert(value);
                        card_offset += CARD_SIZE;
                        continue;
                    }
                }

                let indicator = card.get(8..10).unwrap_or("");
                if indicator != "= " {
                    last_keyword = None;
                    last_had_continuation = false;
                    card_offset += CARD_SIZE;
                    continue;
                }

                let value_area = card.get(10..).unwrap_or("").trim_start();

                if value_area.starts_with('\'') {
                    let (value, has_amp) = parse_quoted_string(value_area);
                    last_keyword = Some(keyword.clone());
                    last_had_continuation = has_amp;
                    keywords.entry(keyword).or_insert(value);
                } else {
                    let value = extract_value_before_comment(value_area);
                    last_keyword = Some(keyword.clone());
                    last_had_continuation = false;
                    keywords.entry(keyword).or_insert(value);
                }

                card_offset += CARD_SIZE;
            }
        }
    }

    /// Parse header from raw bytes (used by tests and in-memory data).
    #[cfg(test)]
    fn parse_header(data: &[u8]) -> Result<Self> {
        let mut cursor = std::io::Cursor::new(data);
        Self::parse_from_reader(&mut cursor)
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
fn parse_fits_f64(s: &str) -> Option<f64> {
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
        let header = FitsHeader { keywords, raw_cards: vec![] };
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
}
