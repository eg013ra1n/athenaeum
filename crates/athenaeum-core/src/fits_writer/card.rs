//! FITS 4.0 header cards: grammar validation + 80-byte record serialization.
//! Fixed-format values (FITS 4.0 §4.2); long strings via the CONTINUE convention (§4.2.1.2).

pub const CARD_SIZE: usize = 80;
pub const BLOCK_SIZE: usize = 2880;
const MAX_STR_CONTENT: usize = 68; // printable chars inside the quotes of one card

#[derive(Debug)]
pub enum FitsWriteError {
    InvalidKeyword(String),
    ReservedKeyword(String),
    NonAsciiString(String),
    CommentTooLong(String),
    ValueTooLong(String),
    NonFiniteReal(String),
    DataSizeMismatch { expected: usize, got: usize },
    BadChannels(usize),
    BadDimensions(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FitsWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKeyword(k) => write!(f, "invalid FITS keyword: {k}"),
            Self::ReservedKeyword(k) => write!(f, "structural keyword not allowed in user cards: {k}"),
            Self::NonAsciiString(k) => write!(f, "non-printable-ASCII string value for {k}"),
            Self::CommentTooLong(k) => write!(f, "comment does not fit the card for {k}"),
            Self::ValueTooLong(k) => write!(f, "value does not fit fixed format for {k}"),
            Self::NonFiniteReal(k) => write!(f, "non-finite real value for {k}"),
            Self::DataSizeMismatch { expected, got } => write!(f, "data length {got}, expected {expected}"),
            Self::BadChannels(c) => write!(f, "channels must be 1 or 3, got {c}"),
            Self::BadDimensions(m) => write!(f, "bad image dimensions: {m}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for FitsWriteError {}
impl From<std::io::Error> for FitsWriteError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CardValue {
    Logical(bool),
    Integer(i64),
    Real(f64),
    Str(String),
}

#[derive(Debug, Clone)]
pub struct Card {
    pub keyword: String,
    pub value: Option<CardValue>, // None => COMMENT/HISTORY-style text card
    pub comment: Option<String>,
    pub(crate) text: Option<String>, // COMMENT/HISTORY payload
}

const RESERVED: [&str; 6] = ["SIMPLE", "BITPIX", "END", "BZERO", "BSCALE", "CONTINUE"];

fn validate_keyword(kw: &str) -> Result<String, FitsWriteError> {
    let up = kw.to_ascii_uppercase();
    if up.is_empty() || up.len() > 8
        || !up.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(FitsWriteError::InvalidKeyword(kw.to_string()));
    }
    if RESERVED.contains(&up.as_str()) || (up.starts_with("NAXIS")) {
        return Err(FitsWriteError::ReservedKeyword(up));
    }
    Ok(up)
}

impl Card {
    pub fn new(keyword: &str, value: CardValue) -> Result<Card, FitsWriteError> {
        Ok(Card { keyword: validate_keyword(keyword)?, value: Some(value), comment: None, text: None })
    }

    pub fn with_comment(mut self, comment: &str) -> Card {
        self.comment = Some(comment.to_string());
        self
    }

    fn text_cards(kind: &str, text: &str) -> Result<Vec<Card>, FitsWriteError> {
        if !is_printable_ascii(text) {
            return Err(FitsWriteError::NonAsciiString(kind.to_string()));
        }
        // Validated ASCII above, so byte-chunking is exact and lossless (one byte per char).
        Ok(text
            .as_bytes()
            .chunks(72)
            .map(|c| Card {
                keyword: kind.to_string(),
                value: None,
                comment: None,
                text: Some(String::from_utf8_lossy(c).into_owned()),
            })
            .collect())
    }
    pub fn comment_cards(text: &str) -> Result<Vec<Card>, FitsWriteError> { Self::text_cards("COMMENT", text) }
    pub fn history_cards(text: &str) -> Result<Vec<Card>, FitsWriteError> { Self::text_cards("HISTORY", text) }

    /// Internal constructor for writer-owned structural cards.
    /// Bypasses only the RESERVED-keyword check (SIMPLE/BITPIX/END/etc. are legitimate here);
    /// charset/length validation still applies via a debug assertion.
    pub(crate) fn structural(keyword: &str, value: CardValue, comment: &str) -> Card {
        debug_assert!(
            !keyword.is_empty()
                && keyword.len() <= 8
                && keyword.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
            "structural keyword {keyword:?} fails FITS charset/length rules"
        );
        Card { keyword: keyword.to_string(), value: Some(value), comment: Some(comment.to_string()), text: None }
    }
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

fn fmt_real(kw: &str, v: f64) -> Result<String, FitsWriteError> {
    if !v.is_finite() {
        return Err(FitsWriteError::NonFiniteReal(kw.to_string()));
    }
    let mut s = format!("{v}");
    if !s.contains('.') { s.push_str(".0"); }
    if s.len() > 20 {
        // {:.10E} always contains a '.'
        s = format!("{v:.10E}");
    }
    if s.len() > 20 {
        return Err(FitsWriteError::ValueTooLong(kw.to_string()));
    }
    Ok(s)
}

fn pack(line: &str) -> [u8; 80] {
    debug_assert!(line.len() <= CARD_SIZE);
    let mut rec = [b' '; 80];
    rec[..line.len()].copy_from_slice(line.as_bytes());
    rec
}

pub fn format_card(card: &Card) -> Result<Vec<[u8; 80]>, FitsWriteError> {
    // Re-validate: Card fields are pub, so constructor-only validation is
    // bypassable. Structural keywords are writer-owned and arrive here via
    // Card::structural — allow exactly those through the reserved check.
    const STRUCTURAL_OK: [&str; 4] = ["SIMPLE", "BITPIX", "NAXIS", "END"];
    let is_structural = STRUCTURAL_OK.contains(&card.keyword.as_str())
        || (card.keyword.starts_with("NAXIS")
            && card.keyword.len() <= 8
            && card.keyword[5..].bytes().all(|b| b.is_ascii_digit())
            && card.comment.is_some()); // structural cards from writer have comment set
    let is_text_kind = card.keyword == "COMMENT" || card.keyword == "HISTORY";
    if !is_structural && !is_text_kind {
        validate_keyword(&card.keyword)?;
    }
    // COMMENT / HISTORY text cards
    if let Some(text) = &card.text {
        if !is_printable_ascii(text) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        return Ok(vec![pack(&format!("{:<8}{}", card.keyword, text))]);
    }

    let Some(value) = card.value.as_ref() else {
        return Err(FitsWriteError::InvalidKeyword(format!(
            "{}: card has neither value nor text", card.keyword
        )));
    };
    let kw8 = format!("{:<8}", card.keyword);

    // Strings get their own path (CONTINUE support)
    if let CardValue::Str(s) = value {
        if !is_printable_ascii(s) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        let escaped = s.replace('\'', "''");
        if escaped.len() <= MAX_STR_CONTENT {
            // fixed format: opening quote col 11, closing quote at/after col 20 => pad to >= 8
            let mut line = format!("{kw8}= '{:<9}'", escaped);
            if let Some(c) = &card.comment {
                if !is_printable_ascii(c) {
                    return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
                }
                let candidate = format!("{line} / {c}");
                if candidate.len() > CARD_SIZE {
                    return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
                }
                line = candidate;
            }
            return Ok(vec![pack(&line)]);
        }
        // CONTINUE chain: each card carries <= 67 content chars + '&' except the last
        let mut records = Vec::new();
        let chars: Vec<char> = escaped.chars().collect();
        let mut idx = 0;
        let mut first = true;
        while idx < chars.len() {
            let take = (chars.len() - idx).min(MAX_STR_CONTENT - 1);
            let chunk: String = chars[idx..idx + take].iter().collect();
            idx += take;
            let cont = idx < chars.len();
            let payload = if cont { format!("{chunk}&") } else { chunk };
            let line = if first {
                first = false;
                format!("{kw8}= '{payload}'")
            } else {
                format!("CONTINUE  '{payload}'")
            };
            if !cont {
                if let Some(c) = &card.comment {
                    if !is_printable_ascii(c) {
                        return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
                    }
                    let candidate = format!("{line} / {c}");
                    if candidate.len() > CARD_SIZE {
                        return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
                    }
                    records.push(pack(&candidate));
                    return Ok(records);
                }
            }
            records.push(pack(&line));
        }
        return Ok(records);
    }

    // fixed-format non-string values right-justified to column 30
    let vstr = match value {
        CardValue::Logical(b) => format!("{:>20}", if *b { "T" } else { "F" }),
        CardValue::Integer(i) => format!("{:>20}", i),
        CardValue::Real(r) => format!("{:>20}", fmt_real(&card.keyword, *r)?),
        CardValue::Str(_) => unreachable!(),
    };
    let mut line = format!("{kw8}= {vstr}");
    if let Some(c) = &card.comment {
        if !is_printable_ascii(c) {
            return Err(FitsWriteError::NonAsciiString(card.keyword.clone()));
        }
        let candidate = format!("{line} / {c}");
        if candidate.len() > CARD_SIZE {
            return Err(FitsWriteError::CommentTooLong(card.keyword.clone()));
        }
        line = candidate;
    }
    Ok(vec![pack(&line)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(records: &[[u8; 80]], i: usize) -> String {
        String::from_utf8(records[i].to_vec()).unwrap()
    }

    #[test]
    fn logical_fixed_format_t_in_col_30() {
        let c = Card::new("SIMPLE2", CardValue::Logical(true)).unwrap();
        let r = format_card(&c).unwrap();
        let line = s(&r, 0);
        assert_eq!(&line[0..8], "SIMPLE2 ");
        assert_eq!(&line[8..10], "= ");
        assert_eq!(line.as_bytes()[29], b'T', "logical value in column 30");
    }

    #[test]
    fn integer_right_justified_to_col_30() {
        let c = Card::new("GAIN", CardValue::Integer(100)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert_eq!(&line[10..30], "                 100");
    }

    #[test]
    fn real_always_has_decimal_point() {
        let c = Card::new("EXPTIME", CardValue::Real(300.0)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line[10..30].contains("300.0"), "got {line:?}");
    }

    #[test]
    fn string_quotes_doubled_and_closing_quote_at_or_after_col_20() {
        let c = Card::new("OBJECT", CardValue::Str("O'Neill".into())).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line.contains("'O''Neill "), "quote doubling + min-8 padding: {line:?}");
    }

    #[test]
    fn long_string_emits_continue_chain() {
        let long = "x".repeat(100);
        let c = Card::new("ATH_SRC", CardValue::Str(long.clone())).unwrap();
        let r = format_card(&c).unwrap();
        assert!(r.len() >= 2);
        let first = s(&r, 0);
        let second = s(&r, 1);
        assert!(first.trim_end().ends_with("&'"), "continuation marker: {first:?}");
        assert!(second.starts_with("CONTINUE  "), "CONTINUE card, no value indicator: {second:?}");
    }

    #[test]
    fn keyword_validation() {
        assert!(Card::new("TOOLONGKEY", CardValue::Integer(1)).is_err()); // 10 chars
        assert!(Card::new("BAD KEY", CardValue::Integer(1)).is_err());    // space
        assert!(Card::new("gain", CardValue::Integer(1)).is_ok());        // lowercase normalized
        assert!(matches!(
            Card::new("NAXIS1", CardValue::Integer(1)),
            Err(FitsWriteError::ReservedKeyword(_))
        ));
    }

    #[test]
    fn non_ascii_rejected() {
        assert!(matches!(
            format_card(&Card::new("OBJECT", CardValue::Str("Туманность".into())).unwrap()),
            Err(FitsWriteError::NonAsciiString(_))
        ));
    }

    #[test]
    fn comment_must_fit() {
        let c = Card::new("GAIN", CardValue::Integer(100)).unwrap()
            .with_comment(&"c".repeat(100));
        assert!(matches!(format_card(&c), Err(FitsWriteError::CommentTooLong(_))));
    }

    #[test]
    fn comment_and_history_cards_split_at_72() {
        let cards = Card::comment_cards(&"y".repeat(100)).unwrap();
        assert_eq!(cards.len(), 2);
        let r = format_card(&cards[0]).unwrap();
        assert!(s(&r, 0).starts_with("COMMENT "));
    }

    #[test]
    fn history_cards_split_at_72() {
        let cards = Card::history_cards(&"z".repeat(100)).unwrap();
        assert_eq!(cards.len(), 2);
        let r = format_card(&cards[0]).unwrap();
        assert!(s(&r, 0).starts_with("HISTORY "));
    }

    #[test]
    fn non_ascii_comment_rejected_on_string_card() {
        let c = Card::new("OBJECT", CardValue::Str("hello".into())).unwrap().with_comment("Ω-neb");
        assert!(matches!(format_card(&c), Err(FitsWriteError::NonAsciiString(_))));
    }

    #[test]
    fn non_ascii_comment_rejected_on_continue_card() {
        let long = "x".repeat(100);
        let c = Card::new("ATH_SRC", CardValue::Str(long)).unwrap().with_comment("Ω-neb");
        assert!(matches!(format_card(&c), Err(FitsWriteError::NonAsciiString(_))));
    }

    #[test]
    fn non_ascii_text_cards_rejected() {
        assert!(matches!(
            Card::comment_cards(&"Ω".repeat(80)),
            Err(FitsWriteError::NonAsciiString(_))
        ));
    }

    #[test]
    fn fmt_real_negative_right_justified_with_point() {
        let c = Card::new("KW", CardValue::Real(-123.456)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line[10..30].ends_with("-123.456"), "got {line:?}");
        assert!(line[10..30].contains('.'), "must have decimal point: {line:?}");
    }

    #[test]
    fn fmt_real_small_magnitude_no_scientific() {
        let c = Card::new("KW", CardValue::Real(1e-8)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line[10..30].ends_with("0.00000001"), "got {line:?}");
    }

    #[test]
    fn fmt_real_large_magnitude_uses_scientific_fallback() {
        let c = Card::new("KW", CardValue::Real(1e20)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        let field = line[10..30].trim_start();
        assert_eq!(field, "1.0000000000E20", "got {line:?}");
        assert!(field.contains('.'), "fallback must have a decimal point: {line:?}");
    }

    #[test]
    fn fmt_real_very_large_negative_fits_within_20_chars() {
        let c = Card::new("KW", CardValue::Real(-1e300)).unwrap();
        let r = format_card(&c);
        assert!(r.is_ok(), "expected -1e300 to fit via scientific fallback: {r:?}");
        let line = s(&r.unwrap(), 0);
        let field = line[10..30].trim_start();
        assert!(field.len() <= 20);
        assert!(field.contains('.'));
    }

    #[test]
    fn fmt_real_zero_has_point() {
        let c = Card::new("KW", CardValue::Real(0.0)).unwrap();
        let line = s(&format_card(&c).unwrap(), 0);
        assert!(line[10..30].ends_with("0.0"), "got {line:?}");
    }
}
