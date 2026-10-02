//! XISF header patching of a COPY (collab v3 wave 6, spec §10.4; plan P5).
//!
//! Monolithic XISF 1.0 only: `XISF0100`, a u32 LE header length, 4 reserved
//! bytes, the XML header, then the attached data blocks at the absolute
//! positions the header's `location="attachment:<pos>:<size>"` attributes
//! name. The XML text is SPLICED at the parser's byte offsets, so every byte
//! outside the edited elements stays identical. When the new header no
//! longer fits before the first block, every block shifts by the smallest
//! multiple of 4096 that makes room and every `location` is rewritten
//! (iterated to a fixed point — the digit count changes the length). Data
//! blocks are copied verbatim (compression and block checksums untouched).
//! A distributed XISF (`path(…)`/`url(…)` locations) is refused.
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::card::FitsWriteError;
use super::header_patch::{commit_tmp, discard_tmp, refuse_same_file, sibling_tmp};
use super::xisf_writer::escape_xml;

const SIGNATURE: &[u8; 8] = b"XISF0100";
const PREFIX: usize = 16;
const ALIGN: u64 = 4096;
const MAX_HEADER: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum XisfEdit {
    /// In every `<Image>`: replace the first `<FITSKeyword name=…>` (later
    /// duplicates dropped); insert it as the Image's first child when absent
    /// and `insert` is true. `value` is FITS-formatted (`'R'` for a string).
    Keyword {
        name: String,
        value: String,
        comment: String,
        insert: bool,
    },
    /// Replace every String `<Property id=…>`; never inserted.
    PropertyIfPresent { id: String, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XisfPatchReport {
    pub images: usize,
    pub shifted_by: u64,
}

struct ImageSite {
    start_tag_end: usize,
    self_closing: Option<Range<usize>>,
    keywords: Vec<(String, Range<usize>)>,
}

struct Parsed {
    images: Vec<ImageSite>,
    properties: Vec<(String, Range<usize>)>,
    /// `(range of the attribute value, pos, size)` of every attachment location.
    locations: Vec<(Range<usize>, u64, u64)>,
}

fn malformed(msg: impl Into<String>) -> FitsWriteError {
    FitsWriteError::Malformed(msg.into())
}

fn attr(e: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, FitsWriteError> {
    for a in e.attributes() {
        let a = a.map_err(|err| malformed(format!("XISF header attribute: {err}")))?;
        if a.key.as_ref() == key {
            let raw = String::from_utf8_lossy(&a.value);
            let text = quick_xml::escape::unescape(&raw)
                .map_err(|err| malformed(format!("XISF header entity: {err}")))?;
            return Ok(Some(text.into_owned()));
        }
    }
    Ok(None)
}

/// Check the tag's `location` (refuse a distributed XISF) and record an
/// attachment's value range; `xml` is the text the tag was parsed from.
fn note_location(
    xml: &str,
    e: &BytesStart<'_>,
    out: &mut Vec<(Range<usize>, u64, u64)>,
) -> Result<(), FitsWriteError> {
    for a in e.attributes() {
        let a = a.map_err(|err| malformed(format!("XISF header attribute: {err}")))?;
        if a.key.as_ref() != b"location" {
            continue;
        }
        let Cow::Borrowed(raw) = &a.value else {
            return Err(malformed("XISF location attribute is not borrowed"));
        };
        let body = std::str::from_utf8(raw)
            .map_err(|err| malformed(format!("XISF location is not UTF-8: {err}")))?;
        let l = body.trim_start();
        if l.starts_with("path(") || l.starts_with("url(") {
            return Err(malformed(
                "distributed XISF (data blocks in external files) is not supported",
            ));
        }
        let Some(rest) = body.strip_prefix("attachment:") else {
            continue;
        };
        let begin = (raw.as_ptr() as usize)
            .checked_sub(xml.as_ptr() as usize)
            .filter(|b| b + raw.len() <= xml.len())
            .ok_or_else(|| malformed("XISF location offset outside the header"))?;
        let mut parts = rest.split(':');
        if let (Some(Ok(p)), Some(Ok(s))) = (
            parts.next().map(|p| p.trim().parse()),
            parts.next().map(|s| s.trim().parse()),
        ) {
            out.push((begin..begin + raw.len(), p, s));
        }
    }
    Ok(())
}

fn parse(xml: &str) -> Result<Parsed, FitsWriteError> {
    let mut reader = Reader::from_str(xml);
    let mut images: Vec<ImageSite> = Vec::new();
    let mut properties = Vec::new();
    let mut locations = Vec::new();
    let mut in_image: Option<usize> = None;
    let mut open_property: Option<(String, usize)> = None;
    loop {
        let start = reader.buffer_position() as usize;
        let ev = reader
            .read_event()
            .map_err(|e| malformed(format!("XISF header XML: {e}")))?;
        let end = reader.buffer_position() as usize;
        match ev {
            Event::Start(e) => {
                note_location(xml, &e, &mut locations)?;
                match e.name().as_ref() {
                    b"Image" => {
                        images.push(ImageSite {
                            start_tag_end: end,
                            self_closing: None,
                            keywords: Vec::new(),
                        });
                        in_image = Some(images.len() - 1);
                    }
                    b"Property" => open_property = attr(&e, b"id")?.map(|id| (id, start)),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                note_location(xml, &e, &mut locations)?;
                match e.name().as_ref() {
                    b"Image" => images.push(ImageSite {
                        start_tag_end: end,
                        self_closing: Some(start..end),
                        keywords: Vec::new(),
                    }),
                    b"FITSKeyword" => {
                        if let (Some(i), Some(name)) = (in_image, attr(&e, b"name")?) {
                            images[i].keywords.push((name, start..end));
                        }
                    }
                    b"Property" => {
                        if let Some(id) = attr(&e, b"id")? {
                            properties.push((id, start..end));
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"Image" => in_image = None,
                b"Property" => {
                    if let Some((id, s)) = open_property.take() {
                        properties.push((id, s..end));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(Parsed {
        images,
        properties,
        locations,
    })
}

/// Every attachment location in `xml`, found through the XML parser.
fn locations(xml: &str) -> Result<Vec<(Range<usize>, u64, u64)>, FitsWriteError> {
    Ok(parse(xml)?.locations)
}

fn keyword_element(name: &str, value: &str, comment: &str) -> String {
    format!(
        "<FITSKeyword name=\"{}\" value=\"{}\" comment=\"{}\"/>",
        escape_xml(name),
        escape_xml(value),
        escape_xml(comment)
    )
}

fn property_element(id: &str, value: &str) -> String {
    format!(
        "<Property id=\"{}\" type=\"String\" value=\"{}\"/>",
        escape_xml(id),
        escape_xml(value)
    )
}

fn apply(xml: &str, parsed: &Parsed, edits: &[XisfEdit]) -> String {
    let mut splices: Vec<(Range<usize>, String)> = Vec::new();
    for img in &parsed.images {
        let mut inserted: Vec<String> = Vec::new();
        for edit in edits {
            let XisfEdit::Keyword {
                name,
                value,
                comment,
                insert,
            } = edit
            else {
                continue;
            };
            let found: Vec<&Range<usize>> = img
                .keywords
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, r)| r)
                .collect();
            match found.split_first() {
                Some((first, rest)) => {
                    splices.push(((*first).clone(), keyword_element(name, value, comment)));
                    for r in rest {
                        splices.push(((*r).clone(), String::new()));
                    }
                }
                None if *insert => inserted.push(keyword_element(name, value, comment)),
                None => {}
            }
        }
        if !inserted.is_empty() {
            let children: String = inserted.iter().map(|el| format!("\n    {el}")).collect();
            match &img.self_closing {
                Some(r) => {
                    let tag = &xml[r.clone()];
                    let open = tag.trim_end().strip_suffix("/>").unwrap_or(tag).trim_end();
                    splices.push((r.clone(), format!("{open}>{children}\n  </Image>")));
                }
                None => splices.push((img.start_tag_end..img.start_tag_end, children)),
            }
        }
    }
    for edit in edits {
        if let XisfEdit::PropertyIfPresent { id, value } = edit {
            for (pid, r) in &parsed.properties {
                if pid == id {
                    splices.push((r.clone(), property_element(id, value)));
                }
            }
        }
    }
    splices.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    let mut out = xml.to_string();
    for (r, text) in splices {
        out.replace_range(r, &text);
    }
    out
}

fn shift(xml: &str, locs: &[(Range<usize>, u64, u64)], delta: u64) -> String {
    let mut out = xml.to_string();
    for (r, p, s) in locs.iter().rev() {
        out.replace_range(r.clone(), &format!("attachment:{}:{}", p + delta, s));
    }
    out
}

/// The XML header (trailing blanks/NULs trimmed) and the stored header length.
fn read_header(f: &mut File) -> Result<(String, usize), FitsWriteError> {
    let mut prefix = [0u8; PREFIX];
    f.read_exact(&mut prefix)
        .map_err(|e| malformed(format!("XISF prefix unreadable: {e}")))?;
    if &prefix[..8] != SIGNATURE {
        return Err(malformed("not a monolithic XISF 1.0 file"));
    }
    let len = u32::from_le_bytes(prefix[8..12].try_into().expect("4 bytes")) as usize;
    if len > MAX_HEADER {
        return Err(malformed("XISF header length exceeds 64 MiB"));
    }
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)
        .map_err(|e| malformed(format!("XISF header truncated: {e}")))?;
    let text =
        String::from_utf8(buf).map_err(|e| malformed(format!("XISF header is not UTF-8: {e}")))?;
    let trimmed = text
        .trim_end_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    Ok((trimmed, len))
}

/// Copy `src` to `dest` with `edits` applied to the XML header.
pub fn patch_xisf_keywords(
    src: &Path,
    dest: &Path,
    edits: &[XisfEdit],
) -> Result<XisfPatchReport, FitsWriteError> {
    refuse_same_file(src, dest)?;
    let mut f = File::open(src)?;
    let (xml, old_len) = read_header(&mut f)?;
    let parsed = parse(&xml)?;
    let edited = apply(&xml, &parsed, edits);
    let locs = locations(&edited)?;
    let first_block = locs.iter().map(|(_, p, _)| *p).min();
    let (out_xml, region, copy_from, delta) = match first_block {
        None => {
            let region = (PREFIX + edited.len()) as u64;
            (edited, region, (PREFIX + old_len) as u64, 0)
        }
        Some(first) => {
            let mut delta = 0u64;
            let mut out = shift(&edited, &locs, 0);
            for _ in 0..8 {
                out = shift(&edited, &locs, delta);
                let need = (PREFIX + out.len()) as u64;
                if need <= first + delta {
                    break;
                }
                delta = (need - first).div_ceil(ALIGN) * ALIGN;
            }
            if (PREFIX + out.len()) as u64 > first + delta {
                return Err(malformed("XISF header shift did not converge"));
            }
            (out, first + delta, first, delta)
        }
    };
    let header_len =
        u32::try_from(region - PREFIX as u64).map_err(|_| malformed("XISF header too large"))?;
    let tmp = sibling_tmp(dest);
    let written = (|| -> Result<(), FitsWriteError> {
        let mut w = BufWriter::new(File::create(&tmp)?);
        w.write_all(SIGNATURE)?;
        w.write_all(&header_len.to_le_bytes())?;
        w.write_all(&[0u8; 4])?;
        w.write_all(out_xml.as_bytes())?;
        let pad = region as usize - PREFIX - out_xml.len();
        w.write_all(&vec![b' '; pad])?;
        f.seek(SeekFrom::Start(copy_from))?;
        std::io::copy(&mut f, &mut w)?;
        w.flush()?;
        w.get_ref().sync_all()?;
        Ok(())
    })();
    match written {
        Ok(()) => {
            commit_tmp(&tmp, dest)?;
            Ok(XisfPatchReport {
                images: parsed.images.len(),
                shifted_by: delta,
            })
        }
        Err(e) => {
            discard_tmp(&tmp, "xisf patch");
            Err(e)
        }
    }
}

/// The first `<Image>`'s `<FITSKeyword name=…>` value (quotes stripped,
/// `''` → `'`, trailing blanks trimmed).
pub fn read_xisf_keyword(path: &Path, name: &str) -> Result<Option<String>, FitsWriteError> {
    let mut f = File::open(path)?;
    let (xml, _) = read_header(&mut f)?;
    let parsed = parse(&xml)?;
    let Some(img) = parsed.images.first() else {
        return Ok(None);
    };
    let Some((_, r)) = img.keywords.iter().find(|(n, _)| n == name) else {
        return Ok(None);
    };
    let mut reader = Reader::from_str(&xml[r.clone()]);
    let ev = reader
        .read_event()
        .map_err(|e| malformed(format!("XISF keyword element: {e}")))?;
    if let Event::Empty(e) = ev {
        if let Some(v) = attr(&e, b"value")? {
            let v = v.trim();
            let v = match v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                Some(inner) => inner.replace("''", "'"),
                None => v.to_string(),
            };
            return Ok(Some(v.trim_end().to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_xisf_f32, Card, CardValue};

    /// A monolithic XISF with the header region ending at 4096; `{POS}`,
    /// `{POS2}` and `{SIZE}` in `xml` are filled in; `data` (and `data2`)
    /// follow at 4096 (and right after).
    fn xisf(xml: &str, data: &[u8], data2: &[u8]) -> Vec<u8> {
        let pos2 = 4096 + data.len();
        let xml = xml
            .replace("{POS2}", &pos2.to_string())
            .replace("{POS}", "4096")
            .replace("{SIZE2}", &data2.len().to_string())
            .replace("{SIZE}", &data.len().to_string());
        let mut out = Vec::new();
        out.extend_from_slice(b"XISF0100");
        out.extend_from_slice(&((4096 - 16) as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(xml.as_bytes());
        out.resize(4096, b' ');
        out.extend_from_slice(data);
        out.extend_from_slice(data2);
        out
    }
    fn blocks(path: &Path) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(path).unwrap();
        let mut f = File::open(path).unwrap();
        let (xml, _) = read_header(&mut f).unwrap();
        locations(&xml)
            .unwrap()
            .into_iter()
            .map(|(_, p, s)| bytes[p as usize..(p + s) as usize].to_vec())
            .collect()
    }
    fn filter_edit(v: &str, comment: &str) -> XisfEdit {
        XisfEdit::Keyword {
            name: "FILTER".into(),
            value: format!("'{v}'"),
            comment: comment.into(),
            insert: true,
        }
    }

    #[test]
    fn sets_the_keyword_in_writer_output_and_keeps_the_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        let card = Card::new("FILTER", CardValue::Str("Red".into())).unwrap();
        write_xisf_f32(&src, 2, 2, 1, &[1.0, 2.0, 3.0, 4.0], &[card]).unwrap();
        let before = std::fs::read(&src).unwrap();
        let dest = dir.path().join("b.xisf");
        let report = patch_xisf_keywords(&src, &dest, &[filter_edit("R", "was 'Red'")]).unwrap();
        assert_eq!(report.shifted_by, 0);
        assert_eq!(
            read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        assert_eq!(blocks(&src), blocks(&dest));
        assert_eq!(std::fs::read(&src).unwrap(), before, "src is never written");
    }

    #[test]
    fn inserts_a_missing_keyword() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.xisf");
        patch_xisf_keywords(&src, &dest, &[filter_edit("Ha", "was unset")]).unwrap();
        assert_eq!(
            read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("Ha")
        );
    }

    #[test]
    fn grows_past_the_first_attachment_and_shifts_every_block() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("two.xisf");
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf version="1.0" xmlns="http://www.pixinsight.com/xisf">
  <Image geometry="2:2:1" sampleFormat="UInt8" location="attachment:{POS}:{SIZE}">
    <FITSKeyword name="FILTER" value="'Red'" comment=""/>
  </Image>
  <Property id="Extra:Blob" type="ByteArray" location="attachment:{POS2}:{SIZE2}"/>
</xisf>
"#;
        std::fs::write(&src, xisf(xml, &[1, 2, 3, 4], &[5, 6, 7])).unwrap();
        let dest = dir.path().join("b.xisf");
        let report =
            patch_xisf_keywords(&src, &dest, &[filter_edit("R", &"x".repeat(6000))]).unwrap();
        assert!(
            report.shifted_by >= 4096 && report.shifted_by % 4096 == 0,
            "{report:?}"
        );
        assert_eq!(
            blocks(&src),
            blocks(&dest),
            "every block byte-identical at its new position"
        );
        assert_eq!(
            read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        let text = crate::fits_parser::extract_xisf_header(&dest).unwrap();
        assert!(
            text.contains("FILTER"),
            "the app's own XISF reader still reads it: {text}"
        );
    }

    #[test]
    fn updates_the_filter_property_in_both_forms() {
        let dir = tempfile::tempdir().unwrap();
        for prop in [
            r#"<Property id="Instrument:Filter:Name" type="String" value="Red"/>"#,
            r#"<Property id="Instrument:Filter:Name" type="String">Red</Property>"#,
        ] {
            let src = dir.path().join("p.xisf");
            let xml = format!(
                "<?xml version=\"1.0\"?>\n<xisf version=\"1.0\">\n  <Image geometry=\"1:1:1\" sampleFormat=\"UInt8\" location=\"attachment:{{POS}}:{{SIZE}}\"/>\n  {prop}\n</xisf>\n"
            );
            std::fs::write(&src, xisf(&xml, &[42], &[])).unwrap();
            let dest = dir.path().join("q.xisf");
            patch_xisf_keywords(
                &src,
                &dest,
                &[
                    filter_edit("R", "was 'Red'"),
                    XisfEdit::PropertyIfPresent {
                        id: "Instrument:Filter:Name".into(),
                        value: "R".into(),
                    },
                ],
            )
            .unwrap();
            let mut f = File::open(&dest).unwrap();
            let (out, _) = read_header(&mut f).unwrap();
            assert!(
                out.contains(r#"id="Instrument:Filter:Name" type="String" value="R""#),
                "{out}"
            );
            assert!(!out.contains(">Red<"));
            assert_eq!(
                read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(),
                Some("R"),
                "self-closing Image got the keyword"
            );
            assert_eq!(blocks(&src), blocks(&dest));
        }
    }

    #[test]
    fn a_distributed_xisf_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("d.xisf");
        let xml = "<?xml version=\"1.0\"?>\n<xisf version=\"1.0\">\n  <Image geometry=\"1:1:1\" sampleFormat=\"UInt8\" location=\"path(@header_dir/d.xisb):0:1\"/>\n</xisf>\n";
        std::fs::write(&src, xisf(xml, &[], &[])).unwrap();
        let err = patch_xisf_keywords(&src, &dir.path().join("e.xisf"), &[filter_edit("R", "")])
            .unwrap_err();
        assert!(err.to_string().contains("distributed XISF"), "{err}");
    }

    #[test]
    fn entities_round_trip_and_same_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 1, 1, 1, &[0.0], &[]).unwrap();
        let dest = dir.path().join("b.xisf");
        patch_xisf_keywords(&src, &dest, &[filter_edit("O&III<1>", "was \"x\"")]).unwrap();
        assert_eq!(
            read_xisf_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("O&III<1>")
        );
        assert!(patch_xisf_keywords(&src, &src, &[])
            .unwrap_err()
            .to_string()
            .contains("onto itself"));
    }

    #[test]
    fn a_location_inside_a_single_quoted_comment_is_not_shifted() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("n.xisf");
        let xml = "<?xml version=\"1.0\"?>\n<xisf version=\"1.0\">\n  <Image geometry=\"1:1:1\" sampleFormat=\"UInt8\" location=\"attachment:{POS}:{SIZE}\">\n    <FITSKeyword name=\"NOTE\" value=\"'x'\" comment='see location=\"attachment:1:2\"'/>\n  </Image>\n</xisf>\n";
        std::fs::write(&src, xisf(xml, &[9], &[])).unwrap();
        let dest = dir.path().join("o.xisf");
        let report =
            patch_xisf_keywords(&src, &dest, &[filter_edit("R", &"x".repeat(6000))]).unwrap();
        assert!(report.shifted_by >= 4096, "{report:?}");
        let mut f = File::open(&dest).unwrap();
        let (out, _) = read_header(&mut f).unwrap();
        assert!(
            out.contains("comment='see location=\"attachment:1:2\"'"),
            "comment untouched"
        );
        assert_eq!(blocks(&src), blocks(&dest));
        let locs = locations(&out).unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].1, 4096 + report.shifted_by);
    }

    #[test]
    fn a_zero_byte_file_is_malformed_for_both_entry_points() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("empty.xisf");
        std::fs::write(&src, b"").unwrap();
        let dest = dir.path().join("out.xisf");
        assert!(patch_xisf_keywords(&src, &dest, &[]).is_err());
        assert!(!dest.exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no tmp left"
        );
        assert!(read_xisf_keyword(&src, "FILTER").is_err());
    }
}
