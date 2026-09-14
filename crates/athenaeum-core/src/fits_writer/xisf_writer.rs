//! Monolithic XISF 1.0 writer (M4d Task 2, ruling R-M4d-3): the same
//! `(width, height, channels, &[f32], &[Card])` surface
//! [`super::writer::write_fits_f32`] takes, serialized as one XISF unit —
//! signature block, XML header, one uncompressed Float32 attachment. No
//! compression, no thumbnails, no properties beyond `XISF:CreationTime` and
//! `XISF:CreatorApplication`.
//!
//! File layout:
//!
//! ```text
//! 0        "XISF0100"                     8 bytes, the format signature
//! 8        header length, u32 little-end  4 bytes
//! 12       reserved, zero                 4 bytes
//! 16       the XML header                 header-length bytes, space-padded
//! 4096·k   the attachment                 width·height·channels·4 bytes
//! ```
//!
//! The declared header length INCLUDES the trailing space padding (trailing
//! whitespace after the root element is well-formed XML), so a reader that
//! computes the data position as `16 + headerLength` lands exactly on the
//! attachment instead of having to trust the `location` attribute alone.
//! The attachment itself starts on a 4096-byte boundary: nothing in the
//! format requires it and the reader this crate ships does not care, but a
//! block-aligned attachment is what other readers (and memory mapping) like,
//! so the header is padded up to the next multiple.
//!
//! The `<Image>` element carries `geometry="w:h:c"`, `sampleFormat="Float32"`,
//! `colorSpace="Gray"` (1 channel) or `"RGB"` (3), `pixelStorage="Planar"`,
//! `bounds="0:1"`, `location="attachment:<offset>:<size>"` and, when the
//! cards' `IMAGETYP` maps onto one of the format's own image-type literals
//! (`Master Light` → `MasterLight`, `Drizzle Weight` → `WeightMap`), an
//! `imageType`. `byteOrder` is left off: its default (little-endian) is what
//! this writer produces.
//!
//! `bounds` is NOT optional. XISF 1.0 §8.5.6: the attribute "shall be
//! specified for all Image elements serializing floating point real pixel
//! data" — there is no default representable range for a Float32 image,
//! and the external tool refuses the file outright without it ("Missing
//! bounds Image attribute, which is mandatory for a floating point real
//! image", the v0.6.2 masters). The v0.6.2 writer left it off on the belief
//! that `0:1` was the format's default; that was only OUR reader's
//! default. The declared range is the pipeline's own convention — every
//! master's samples are the u16 domain divided by 65535 (`ATH_CSCL`), so
//! `0:1` names the black and white points the array was built against, and
//! the reader this crate ships treats an explicit `0:1` as the identity
//! mapping it already applied. The range is a REPRESENTABLE range, not a
//! clip: a saturated star core above 1.0 is a sample beyond the white
//! point, which the format allows. Declaring the array's true min/max
//! instead would oblige every honouring reader to rescale the data, which
//! is worse.
//!
//! Samples are little-endian f32 in the SAME plane-major order the FITS
//! writer uses (all of channel 0, then 1, then 2), which is what
//! `pixelStorage="Planar"` declares — no reordering, no scaling.
//!
//! **Row order is a known limitation** (M4d Task 2 fix round 1, ruling
//! R-T2-1). XISF's convention is that row 0 IS the top row and no XISF
//! reader has a `ROWORDER` concept, but the array handed to this writer is
//! simply its source frames' order — no stage of the stacking pipeline
//! flips pixels, and a stacking master's `ROWORDER` is its source frames'
//! own, copied through calibration, registration and the master's card
//! build. So for a BOTTOM-UP set, or one whose cards carry no `ROWORDER`
//! at all (which the astronomical convention reads as bottom-up —
//! `crate::orientation`), an XISF viewer shows this file vertically
//! mirrored relative to the FITS file of
//! the same data. The pixels are deliberately NOT flipped here: a row flip
//! would have to transform the master's WCS too (`CRPIX2`, the CD matrix,
//! the odd-`v` SIP terms), which is a feature of its own and is recorded as
//! a follow-up. The WCS is untouched and stays correct for the array as
//! stored. What this writer does instead is state the effective order
//! EXPLICITLY in the keyword list — the copied value when the cards carry
//! one, `'BOTTOM-UP'` when they do not — so a reader is never left to
//! guess, and the stacking run warns once per bottom-up XISF master.
//!
//! **Reading one of these back is not symmetric.** rustafits' XISF reader
//! returns every float sample multiplied by 65535 (its ADU-domain
//! convention, ruling R-M4a-11) — a caller comparing pixels against what it
//! wrote must divide by 65535 first.
//!
//! Header cards are emitted as `<FITSKeyword name value comment>` children
//! of the `<Image>`, with values formatted by the same rules
//! [`super::card::format_card`] uses (so both containers of one master say
//! the same thing) minus the fixed-format column padding, which is a FITS
//! card-layout artifact with no meaning in XML — see [`xisf_keyword_value`].
//! Every card is run through `format_card` before anything is written, so
//! this writer accepts exactly the card set the FITS writer accepts and a
//! master can never carry a card in one container that the other refused.
//! The structural cards (`SIMPLE`/`BITPIX`/`NAXIS*`) are NOT emitted: the
//! `<Image>` attributes above carry that information, and the FITS writer
//! owns those cards itself rather than taking them from `cards`.
//!
//! **Not byte-reproducible.** `XISF:CreationTime` is the wall clock, so two
//! writes of the same image and cards differ in the header (the attachment
//! is identical). A FITS write of the same data IS byte-identical run to
//! run, so every byte-comparison pin and every acceptance-run byte
//! comparison stays on `output.format = fits`.

use std::borrow::Cow;
use std::io::Write;
use std::path::Path;

use super::card::{fmt_real, format_card, sanitize_text, Card, CardValue, FitsWriteError};
use super::writer::{rename_replace, validate};

/// The format signature: "XISF" plus the version it declares, 1.0.0.
const SIGNATURE: &[u8; 8] = b"XISF0100";

/// Signature (8) + header length (4) + reserved (4).
const SIGNATURE_BLOCK: usize = 16;

/// The attachment starts on a multiple of this — see the module docs.
const ATTACHMENT_ALIGN: usize = 4096;

/// The XISF namespace is the format's own schema identifier, written
/// verbatim as every XISF header must: a URI naming the XML schema this
/// document conforms to, not a reference to any application.
const XISF_NS: &str = "http://www.pixinsight.com/xisf";
const XSD_LOCATION: &str = "http://www.pixinsight.com/xisf http://pixinsight.com/xisf/xisf-1.0.xsd";

/// `XISF:CreatorApplication` (ruling R-M4d-3).
const CREATOR: &str = concat!("Athenaeum ", env!("CARGO_PKG_VERSION"));

/// One header card as the pair of strings an XISF `<FITSKeyword>` element
/// carries: `(value, comment)`.
///
/// The value is formatted by the FITS rules — a logical is `T`/`F`, an
/// integer is decimal, a real always carries a decimal point
/// (`super::card`'s own `fmt_real`, scientific fallback included), a string
/// is wrapped in single quotes with any embedded quote doubled — with the
/// fixed-format column padding dropped: FITS right-justifies a value to
/// column 30 and pads a short string to 8 characters inside its quotes
/// purely to fill an 80-byte record, and neither has any meaning in an XML
/// attribute. So `EXPTIME` is `180.0`, not 15 spaces then `180.0`, and
/// `OBJECT` is `'LDN 1272'`, not `'LDN 1272 '`.
///
/// A COMMENT/HISTORY-style text card has no value: the sanitized text comes
/// back as the comment, which is the same shape a FITS reader reports for
/// those keywords.
///
/// Non-ASCII characters degrade to `?` exactly as they do on a FITS card
/// (`sanitize_text`) even though XML could carry them: the two containers of
/// one master must not disagree about what a card says. That sanitizer logs
/// a warning, and [`write_xisf_f32`]'s card-parity check runs the same card
/// through `format_card`, so an offending card warns twice per write.
///
/// Infallible by contract, so a non-finite real — which the FITS writer
/// refuses outright — falls back to Rust's own formatting of the value.
/// [`write_xisf_f32`] rejects such a card before this is ever reached.
pub fn xisf_keyword_value(card: &Card) -> (String, String) {
    if let Some(text) = &card.text {
        return (
            String::new(),
            sanitize_text(&card.keyword, text).into_owned(),
        );
    }
    let value = match card.value.as_ref() {
        Some(CardValue::Logical(b)) => (if *b { "T" } else { "F" }).to_string(),
        Some(CardValue::Integer(i)) => i.to_string(),
        Some(CardValue::Real(r)) => fmt_real(&card.keyword, *r).unwrap_or_else(|_| format!("{r}")),
        Some(CardValue::Str(s)) => {
            format!("'{}'", sanitize_text(&card.keyword, s).replace('\'', "''"))
        }
        None => String::new(),
    };
    let comment = card
        .comment
        .as_deref()
        .map(|c| sanitize_text(&card.keyword, c).into_owned())
        .unwrap_or_default();
    (value, comment)
}

/// XML attribute escaping. `'` is deliberately left alone — every attribute
/// here is delimited by `"`, and FITS string values are full of single
/// quotes, so escaping them would only make the header unreadable.
fn escape_xml(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"')) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn align_up(n: usize, a: usize) -> usize {
    n.div_ceil(a) * a
}

/// `ROWORDER`, made explicit (M4d Task 2 fix round 1, ruling R-T2-1 — see
/// the module docs): XISF has no row-order concept of its own, so the
/// keyword list must say which order the stored array is actually in rather
/// than leave a reader with the FITS convention's silent default.
///
/// A card list that already carries `ROWORDER` is returned borrowed and its
/// value passes through verbatim — it IS the answer, and rewriting it would
/// only invent a disagreement with the FITS twin. A list without one gets
/// [`crate::orientation::ROW_ORDER_BOTTOM_UP`] appended, which is the same
/// astronomical default `crate::orientation::row_order_is_bottom_up`
/// applies to a missing card (that function stays the ONE encoding of the
/// rule; this is its `None` arm, spelled with its own constant).
fn with_explicit_row_order(cards: &[Card]) -> Result<Cow<'_, [Card]>, FitsWriteError> {
    if cards.iter().any(|c| c.keyword == "ROWORDER") {
        return Ok(Cow::Borrowed(cards));
    }
    let mut owned = cards.to_vec();
    owned.push(
        Card::new(
            "ROWORDER",
            CardValue::Str(crate::orientation::ROW_ORDER_BOTTOM_UP.to_string()),
        )?
        .with_comment("image row order (astronomical default)"),
    );
    Ok(Cow::Owned(owned))
}

/// The representable range every master this writer serializes was built
/// against: the u16 domain divided by 65535 (`ATH_CSCL`), i.e. `[0, 1]`.
/// Mandatory on a Float32 `<Image>` (XISF 1.0 §8.5.6).
const FLOAT_BOUNDS: &str = "0:1";

/// The `imageType` literal (XISF 1.0 table 12) for the cards' `IMAGETYP`,
/// when the two vocabularies coincide: the master light and the drizzle
/// weight map have exact counterparts; everything else (the rejection maps
/// are FITS-only anyway) gets no attribute rather than a guessed one.
fn image_type_for(cards: &[Card]) -> Option<&'static str> {
    let imagetyp = cards.iter().find(|c| c.keyword == "IMAGETYP")?;
    let Some(CardValue::Str(s)) = &imagetyp.value else {
        return None;
    };
    match s.trim() {
        "Master Light" => Some("MasterLight"),
        "Drizzle Weight" => Some("WeightMap"),
        _ => None,
    }
}

/// The XML header for one image, with the attachment at `attachment_pos`.
fn build_xml(
    width: usize,
    height: usize,
    channels: usize,
    attachment_pos: usize,
    data_bytes: usize,
    cards: &[Card],
    creation_time: &str,
) -> String {
    let color_space = if channels == 3 { "RGB" } else { "Gray" };
    let image_type_attr = image_type_for(cards)
        .map(|t| format!(" imageType=\"{t}\""))
        .unwrap_or_default();
    let mut xml = String::with_capacity(512 + cards.len() * 96);
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<xisf version=\"1.0\" xmlns=\"{XISF_NS}\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xsi:schemaLocation=\"{XSD_LOCATION}\">\n"
    ));
    // `bounds` is mandatory for a floating point real image (see the
    // module docs) — the pipeline's `[0, 1]` convention, stated explicitly.
    xml.push_str(&format!(
        "  <Image geometry=\"{width}:{height}:{channels}\" sampleFormat=\"Float32\" \
         colorSpace=\"{color_space}\" pixelStorage=\"Planar\" \
         bounds=\"{FLOAT_BOUNDS}\"{image_type_attr} \
         location=\"attachment:{attachment_pos}:{data_bytes}\">\n"
    ));
    for card in cards {
        let (value, comment) = xisf_keyword_value(card);
        xml.push_str(&format!(
            "    <FITSKeyword name=\"{}\" value=\"{}\" comment=\"{}\"/>\n",
            escape_xml(&card.keyword),
            escape_xml(&value),
            escape_xml(&comment)
        ));
    }
    xml.push_str("  </Image>\n");
    xml.push_str("  <Metadata>\n");
    xml.push_str(&format!(
        "    <Property id=\"XISF:CreationTime\" type=\"TimePoint\" value=\"{}\"/>\n",
        escape_xml(creation_time)
    ));
    xml.push_str(&format!(
        "    <Property id=\"XISF:CreatorApplication\" type=\"String\" value=\"{}\"/>\n",
        escape_xml(CREATOR)
    ));
    xml.push_str("  </Metadata>\n");
    xml.push_str("</xisf>\n");
    xml
}

/// Signature block + XML header + space padding: everything before the
/// attachment, whose own offset is therefore `prefix.len()`.
///
/// The `location` attribute names that offset, which depends on the header's
/// length, which depends on the digits of the offset — so solve for it:
/// start one block in and re-derive until the padded header ends exactly
/// where the attachment starts. Each retry can only make the number longer,
/// never shorter, so this settles in a round or two; the bounded loop keeps
/// a pathological input from spinning instead of failing.
fn header_prefix(
    width: usize,
    height: usize,
    channels: usize,
    data_bytes: usize,
    cards: &[Card],
    creation_time: &str,
) -> Result<Vec<u8>, FitsWriteError> {
    let mut pos = ATTACHMENT_ALIGN;
    for _ in 0..8 {
        let xml = build_xml(
            width,
            height,
            channels,
            pos,
            data_bytes,
            cards,
            creation_time,
        );
        let need = align_up(SIGNATURE_BLOCK + xml.len(), ATTACHMENT_ALIGN);
        if need != pos {
            pos = need;
            continue;
        }
        let declared = u32::try_from(pos - SIGNATURE_BLOCK).map_err(|_| {
            FitsWriteError::Malformed(format!("XISF header of {} bytes is too large", pos))
        })?;
        let mut out = Vec::with_capacity(pos);
        out.extend_from_slice(SIGNATURE);
        out.extend_from_slice(&declared.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(xml.as_bytes());
        out.resize(pos, b' ');
        return Ok(out);
    }
    Err(FitsWriteError::Malformed(
        "XISF header length did not settle on an attachment offset".to_string(),
    ))
}

/// Write an XISF file at `path`, replacing any existing file only after the
/// write fully succeeds — [`super::writer::write_fits_f32`]'s contract and
/// mechanism exactly: validate first (so a bad call never touches `path`),
/// write to a sibling temp file, `sync_all`, then atomically rename it into
/// place, so a pre-existing good file is never truncated by a failed write.
pub fn write_xisf_f32(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    validate(width, height, channels, data.len())?;

    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        path.with_extension(format!("xisf.tmp.{}.{}", std::process::id(), seq))
    };
    let write_result = (|| -> Result<(), FitsWriteError> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        write_xisf_f32_to(&mut w, width, height, channels, data, cards)?;
        w.flush()?;
        // Power-loss durability: data must be on disk before the rename
        // makes the file visible under its final name.
        w.get_ref().sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = rename_replace(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// [`write_xisf_f32`]'s streaming body — the whole unit, header and
/// attachment, into one sink.
pub fn write_xisf_f32_to<W: Write>(
    mut w: W,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    validate(width, height, channels, data.len())?;
    // The stored array's row order, stated explicitly (ruling R-T2-1).
    let cards = with_explicit_row_order(cards)?;
    // Card-grammar parity with the FITS writer (see the module docs): an
    // invalid or reserved keyword, a non-finite real or an over-long comment
    // fails an XISF write exactly as it fails a FITS one, so the two
    // containers of one master always carry the same cards. Runs over the
    // FINAL list, the added `ROWORDER` included.
    for card in cards.iter() {
        format_card(card)?;
    }

    let data_bytes = data.len() * std::mem::size_of::<f32>();
    let creation_time = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let prefix = header_prefix(width, height, channels, data_bytes, &cards, &creation_time)?;
    w.write_all(&prefix)?;

    let mut buf = Vec::with_capacity(8192 * 4);
    for v in data {
        buf.extend_from_slice(&v.to_le_bytes());
        if buf.len() >= 8192 * 4 {
            w.write_all(&buf)?;
            buf.clear();
        }
    }
    w.write_all(&buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The brief's four card shapes: a string, a real, an integer and a
    /// logical (`SIMPLE` itself is reserved, so the logical is `ATH_TEST`).
    fn sample_cards() -> Vec<Card> {
        vec![
            Card::new("OBJECT", CardValue::Str("LDN 1272".into()))
                .unwrap()
                .with_comment("target"),
            Card::new("EXPTIME", CardValue::Real(180.0))
                .unwrap()
                .with_comment("total exposure [s]"),
            Card::new("ATH_STKN", CardValue::Integer(208)).unwrap(),
            Card::new("ATH_TEST", CardValue::Logical(true)).unwrap(),
        ]
    }

    fn ramp(width: usize, height: usize, channels: usize) -> Vec<f32> {
        (0..width * height * channels)
            .map(|i| i as f32 / 100.0)
            .collect()
    }

    /// Signature block + XML, as a reader sees them.
    fn split_header(bytes: &[u8]) -> (usize, String) {
        assert_eq!(&bytes[..8], SIGNATURE, "signature");
        let declared = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        assert_eq!(
            &bytes[12..16],
            &[0u8; 4],
            "the 4 reserved bytes must be zero"
        );
        let xml = String::from_utf8(bytes[16..16 + declared].to_vec()).unwrap();
        (declared, xml)
    }

    #[test]
    fn header_declares_geometry_keywords_and_an_aligned_attachment() {
        let (w, h, ch) = (5usize, 3usize, 3usize);
        let data = ramp(w, h, ch);
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, w, h, ch, &data, &sample_cards()).unwrap();

        let (declared, xml) = split_header(&out);
        let attachment_pos = SIGNATURE_BLOCK + declared;
        assert_eq!(
            attachment_pos % ATTACHMENT_ALIGN,
            0,
            "the attachment must start on a {ATTACHMENT_ALIGN}-byte boundary, got {attachment_pos}"
        );
        assert_eq!(
            out.len(),
            attachment_pos + data.len() * 4,
            "total file size"
        );

        assert!(
            xml.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"),
            "{xml}"
        );
        assert!(xml.contains("geometry=\"5:3:3\""), "{xml}");
        assert!(xml.contains("sampleFormat=\"Float32\""), "{xml}");
        assert!(xml.contains("colorSpace=\"RGB\""), "{xml}");
        assert!(xml.contains("pixelStorage=\"Planar\""), "{xml}");
        // v0.6.3: mandatory for a Float32 image (XISF 1.0 §8.5.6) — the
        // v0.6.2 masters lacked it and the external tool refused them.
        assert!(xml.contains("bounds=\"0:1\""), "{xml}");
        assert!(
            xml.contains(&format!(
                "location=\"attachment:{attachment_pos}:{}\"",
                data.len() * 4
            )),
            "{xml}"
        );
        assert!(
            xml.contains("<FITSKeyword name=\"OBJECT\" value=\"'LDN 1272'\""),
            "{xml}"
        );
        assert!(xml.contains("name=\"EXPTIME\" value=\"180.0\""), "{xml}");
        assert!(xml.contains("name=\"ATH_STKN\" value=\"208\""), "{xml}");
        assert!(xml.contains("name=\"ATH_TEST\" value=\"T\""), "{xml}");
        assert!(xml.contains("id=\"XISF:CreatorApplication\""), "{xml}");
        assert!(xml.contains("id=\"XISF:CreationTime\""), "{xml}");

        // The samples, little-endian f32, plane-major, unscaled.
        let attachment = &out[attachment_pos..];
        for (i, expected) in data.iter().enumerate() {
            let off = i * 4;
            let got = f32::from_le_bytes([
                attachment[off],
                attachment[off + 1],
                attachment[off + 2],
                attachment[off + 3],
            ]);
            assert_eq!(got, *expected, "sample {i}");
        }
    }

    /// Ruling R-T2-1: the keyword list always states the stored array's row
    /// order — the source's own value when it has one, the astronomical
    /// default when it does not, and never two `ROWORDER` cards.

    /// `imageType` (XISF 1.0 table 12) follows the cards' `IMAGETYP` only
    /// where the two vocabularies coincide — a master light and a drizzle
    /// weight map — and is absent, never guessed, for anything else.
    #[test]
    fn image_type_follows_imagetyp_where_the_format_names_it() {
        let (w, h, ch) = (4usize, 2usize, 1usize);
        let data = ramp(w, h, ch);
        let write = |imagetyp: Option<&str>| -> String {
            let mut cards = Vec::new();
            if let Some(t) = imagetyp {
                cards.push(Card::new("IMAGETYP", CardValue::Str(t.to_string())).unwrap());
            }
            let mut out: Vec<u8> = Vec::new();
            write_xisf_f32_to(&mut out, w, h, ch, &data, &cards).unwrap();
            split_header(&out).1
        };
        assert!(write(Some("Master Light")).contains("imageType=\"MasterLight\""));
        assert!(write(Some("Drizzle Weight")).contains("imageType=\"WeightMap\""));
        assert!(!write(Some("Rejection Map Low")).contains("imageType="));
        assert!(!write(None).contains("imageType="));
        // Every one of them still declares the mandatory range.
        assert!(write(None).contains("bounds=\"0:1\""));
    }
    #[test]
    fn row_order_is_always_stated_explicitly() {
        let with = vec![Card::new(
            "ROWORDER",
            CardValue::Str(crate::orientation::ROW_ORDER_TOP_DOWN.into()),
        )
        .unwrap()];
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &with).unwrap();
        let (_, xml) = split_header(&out);
        assert!(
            xml.contains("name=\"ROWORDER\" value=\"'TOP-DOWN'\""),
            "a TOP-DOWN source passes through verbatim: {xml}"
        );
        assert_eq!(
            xml.matches("name=\"ROWORDER\"").count(),
            1,
            "exactly one ROWORDER keyword: {xml}"
        );

        // No ROWORDER card at all: the same astronomical default
        // `orientation::row_order_is_bottom_up` applies to a missing card.
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &sample_cards()).unwrap();
        let (_, xml) = split_header(&out);
        assert!(
            xml.contains("name=\"ROWORDER\" value=\"'BOTTOM-UP'\""),
            "a source with no ROWORDER is declared bottom-up: {xml}"
        );
        assert_eq!(xml.matches("name=\"ROWORDER\"").count(), 1, "{xml}");

        // An unrecognized explicit value is still the source's answer and
        // passes through untouched (the rule reads it as bottom-up for the
        // run's warning, which is a separate question).
        let odd = vec![Card::new("ROWORDER", CardValue::Str("sideways".into())).unwrap()];
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &odd).unwrap();
        let (_, xml) = split_header(&out);
        assert!(
            xml.contains("name=\"ROWORDER\" value=\"'sideways'\""),
            "{xml}"
        );
    }

    #[test]
    fn one_channel_is_gray() {
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &[]).unwrap();
        let (_, xml) = split_header(&out);
        assert!(xml.contains("colorSpace=\"Gray\""), "{xml}");
        assert!(xml.contains("geometry=\"4:2:1\""), "{xml}");
    }

    /// R-M4d-3's own acceptance: the file the writer produces is read back
    /// by the reader this project ships. `read_raw` returns floats in the
    /// ADU domain (R-M4a-11, ×65535), so divide before comparing; ×65535
    /// then ÷65535 costs at most an ulp, hence the 1e-7 tolerance rather
    /// than bit equality.
    #[cfg(feature = "render")]
    #[test]
    fn round_trips_through_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.xisf");
        let (w, h, ch) = (5usize, 3usize, 3usize);
        let data = ramp(w, h, ch);
        write_xisf_f32(&path, w, h, ch, &data, &sample_cards()).unwrap();

        let (meta, pixels) = astroimage::ImageConverter::read_raw(&path).unwrap();
        assert_eq!((meta.width, meta.height, meta.channels), (w, h, ch));
        let samples = match pixels {
            astroimage::PixelData::Float32(v) => v,
            _ => panic!("expected Float32 samples"),
        };
        assert_eq!(samples.len(), data.len());
        for (i, (got, expected)) in samples.iter().zip(data.iter()).enumerate() {
            let got = got / 65535.0;
            assert!(
                (got - expected).abs() <= 1e-7,
                "sample {i}: read {got}, wrote {expected}"
            );
        }
    }

    #[test]
    fn keyword_values_drop_the_fits_column_padding_but_nothing_else() {
        let cards = sample_cards();
        let pairs: Vec<(String, String)> = cards.iter().map(xisf_keyword_value).collect();
        assert_eq!(pairs[0], ("'LDN 1272'".to_string(), "target".to_string()));
        assert_eq!(
            pairs[1],
            ("180.0".to_string(), "total exposure [s]".to_string())
        );
        assert_eq!(pairs[2], ("208".to_string(), String::new()));
        assert_eq!(pairs[3], ("T".to_string(), String::new()));

        // A quote inside a string is doubled, FITS-style.
        let quoted = Card::new("OBSERVER", CardValue::Str("O'Neill".into())).unwrap();
        assert_eq!(xisf_keyword_value(&quoted).0, "'O''Neill'");

        // A COMMENT text card has no value; the text is the comment.
        let comment = Card::comment_cards("stacked by Athenaeum").unwrap();
        assert_eq!(
            xisf_keyword_value(&comment[0]),
            (String::new(), "stacked by Athenaeum".to_string())
        );

        // Non-ASCII degrades to '?' exactly as it does on a FITS card.
        let cyrillic = Card::new("OBJECT", CardValue::Str("Туманность".into())).unwrap();
        assert_eq!(xisf_keyword_value(&cyrillic).0, "'??????????'");
    }

    /// The agreement claim in the module docs, pinned: a numeric or logical
    /// XISF value IS the FITS card's value field with the right-justifying
    /// spaces gone; a string value is the FITS card's quoted field with the
    /// 8-character pad inside the quotes gone. Nothing else differs.
    #[test]
    fn values_match_the_fits_card_field() {
        for card in sample_cards() {
            let record = format_card(&card).unwrap();
            assert_eq!(record.len(), 1, "{}: one record expected", card.keyword);
            let line = String::from_utf8(record[0].to_vec()).unwrap();
            let xisf = xisf_keyword_value(&card).0;
            let expected = match card.value.as_ref() {
                Some(CardValue::Str(_)) => {
                    // A quoted value runs from column 11 to its closing
                    // quote, which a comment can push past column 30 — so
                    // slice to that quote rather than to a fixed column.
                    let rest = &line[10..];
                    let end = rest.rfind('\'').expect("a closing quote");
                    let inner = rest[..=end].trim_matches('\'');
                    format!("'{}'", inner.trim_end())
                }
                // Fixed-format non-string values are right-justified into
                // columns 11..30.
                _ => line[10..30].trim().to_string(),
            };
            assert_eq!(xisf, expected, "{}", card.keyword);
        }
    }

    #[test]
    fn card_grammar_parity_with_the_fits_writer() {
        // A non-finite real: refused, same as on the FITS path.
        let nan = Card::new("ATH_BAD", CardValue::Real(f64::NAN)).unwrap();
        let r = write_xisf_f32_to(std::io::sink(), 2, 2, 1, &[0.0; 4], &[nan]);
        assert!(matches!(r, Err(FitsWriteError::NonFiniteReal(_))), "{r:?}");

        // A hand-built card claiming a structural keyword: refused, because
        // `format_card` re-validates every non-structural card.
        let reserved = Card {
            keyword: "NAXIS1".to_string(),
            value: Some(CardValue::Integer(1)),
            comment: None,
            text: None,
            structural: false,
        };
        let r = write_xisf_f32_to(std::io::sink(), 2, 2, 1, &[0.0; 4], &[reserved]);
        assert!(
            matches!(r, Err(FitsWriteError::ReservedKeyword(_))),
            "{r:?}"
        );

        // An over-long comment: refused for FITS-card parity (documented).
        let long = Card::new("GAIN", CardValue::Integer(100))
            .unwrap()
            .with_comment(&"c".repeat(100));
        let r = write_xisf_f32_to(std::io::sink(), 2, 2, 1, &[0.0; 4], &[long]);
        assert!(matches!(r, Err(FitsWriteError::CommentTooLong(_))), "{r:?}");
    }

    #[test]
    fn geometry_validated_before_any_io() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.xisf");

        let r = write_xisf_f32(&path, 4, 2, 2, &[0.0; 16], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadChannels(2))), "{r:?}");
        let r = write_xisf_f32(&path, 4, 2, 1, &[0.0; 7], &[]);
        assert!(
            matches!(
                r,
                Err(FitsWriteError::DataSizeMismatch {
                    expected: 8,
                    got: 7
                })
            ),
            "{r:?}"
        );
        let r = write_xisf_f32(&path, 0, 2, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
        assert!(
            !path.exists(),
            "a rejected call must not have touched the target path"
        );
    }

    /// A header big enough to push the attachment past one block still ends
    /// exactly on a boundary, with the `location` offset agreeing.
    #[test]
    fn a_long_header_pushes_the_attachment_to_the_next_block() {
        let cards: Vec<Card> = (0..200)
            .map(|i| {
                // 40 chars + a 23-char comment is the most an 80-byte FITS
                // card holds, and the card-parity check refuses anything
                // longer — 200 of these still overflow one 4096-byte block.
                Card::new(&format!("ATH_{i:03}"), CardValue::Str("x".repeat(40)))
                    .unwrap()
                    .with_comment("a card that costs bytes")
            })
            .collect();
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &cards).unwrap();
        let (declared, xml) = split_header(&out);
        let attachment_pos = SIGNATURE_BLOCK + declared;
        assert!(
            attachment_pos > ATTACHMENT_ALIGN,
            "expected a header past one block, got {attachment_pos}"
        );
        assert_eq!(attachment_pos % ATTACHMENT_ALIGN, 0, "{attachment_pos}");
        assert!(
            xml.contains(&format!("location=\"attachment:{attachment_pos}:32\"")),
            "the location offset must name the real attachment position"
        );
        assert_eq!(out.len(), attachment_pos + 32);
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        let card = Card::new("ATH_XML", CardValue::Str("a<b>c&d\"e".into()))
            .unwrap()
            .with_comment("1 < 2 & 3 > 2");
        let mut out: Vec<u8> = Vec::new();
        write_xisf_f32_to(&mut out, 4, 2, 1, &ramp(4, 2, 1), &[card]).unwrap();
        let (_, xml) = split_header(&out);
        assert!(
            xml.contains("value=\"'a&lt;b&gt;c&amp;d&quot;e'\""),
            "{xml}"
        );
        assert!(xml.contains("comment=\"1 &lt; 2 &amp; 3 &gt; 2\""), "{xml}");
    }
}
