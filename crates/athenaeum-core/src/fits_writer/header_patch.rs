//! Byte-level FITS header patching of a COPY (collab v3 wave 6, spec §10.3).
//!
//! [`patch_cards`] copies `src` to `dest` HDU by HDU: each header is
//! re-rendered with the edits applied, each data unit — and anything after
//! the last HDU — is streamed verbatim (no pixel decode). `src` is opened
//! read-only and `src == dest` is refused: the project export patches a copy
//! of a landed replica, never the replica itself.
//!
//! Edit semantics (plan ruling P4): `Set` applies to the image HDU (inserted
//! before END when absent) and to every other HDU that already carries the
//! keyword; `SetIfPresent` only where it exists; `Remove` everywhere. The
//! first card of a keyword is replaced in place; later duplicates go.
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::card::{format_card, Card, FitsWriteError, BLOCK_SIZE, CARD_SIZE};

type Record = [u8; CARD_SIZE];

/// The most header blocks one HDU may span before the file is malformed.
const MAX_HEADER_BLOCKS: usize = 64;

/// One header edit (plan ruling P4).
#[derive(Debug, Clone)]
pub enum CardEdit {
    Set(Card),
    SetIfPresent(Card),
    Remove(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchReport {
    pub hdus: usize,
    /// The HDU that holds the image (0 = primary).
    pub image_hdu: usize,
    /// Header blocks added across all HDUs.
    pub grown_blocks: usize,
}

struct HduLayout {
    cards: Vec<Record>,
    header_blocks: usize,
    data_offset: u64,
    data_len_padded: u64,
}

fn keyword_of(rec: &Record) -> &str {
    std::str::from_utf8(&rec[..8]).unwrap_or("").trim_end()
}

fn is_value_card(rec: &Record) -> bool {
    &rec[8..10] == b"= "
}

/// `(start, len)` of every value card of `key`, each with its CONTINUE tail.
fn spans_of(cards: &[Record], key: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < cards.len() {
        if keyword_of(&cards[i]) == key && is_value_card(&cards[i]) {
            let mut len = 1;
            while i + len < cards.len() && keyword_of(&cards[i + len]) == "CONTINUE" {
                len += 1;
            }
            out.push((i, len));
            i += len;
        } else {
            i += 1;
        }
    }
    out
}

/// The first `key` card's value: a string unquoted (`''` → `'`, CONTINUE
/// records joined, trailing blanks trimmed); anything else the trimmed text
/// before a `/` comment.
fn value_of(cards: &[Record], key: &str) -> Option<String> {
    let (start, len) = *spans_of(cards, key).first()?;
    let head = String::from_utf8_lossy(&cards[start][10..]).into_owned();
    if !head.trim_start().starts_with('\'') {
        return Some(head.split('/').next().unwrap_or("").trim().to_string());
    }
    let mut out = String::new();
    for rec in &cards[start..start + len] {
        let text = String::from_utf8_lossy(&rec[10..]).into_owned();
        let Some(rest) = text.trim_start().strip_prefix('\'') else {
            break;
        };
        let mut piece = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\'' {
                piece.push(c);
            } else if chars.peek() == Some(&'\'') {
                chars.next();
                piece.push('\'');
            } else {
                break;
            }
        }
        match piece.strip_suffix('&') {
            Some(cont) => out.push_str(cont),
            None => {
                out.push_str(&piece);
                break;
            }
        }
    }
    Some(out.trim_end().to_string())
}

fn int_of(cards: &[Record], key: &str) -> Option<i64> {
    value_of(cards, key)?.parse().ok()
}

fn malformed(msg: impl Into<String>) -> FitsWriteError {
    FitsWriteError::Malformed(msg.into())
}

/// The padded byte length of an HDU's data unit (FITS 4.0 §4.4.1.1).
fn data_len_padded(cards: &[Record]) -> Result<u64, FitsWriteError> {
    let bitpix = int_of(cards, "BITPIX").ok_or_else(|| malformed("an HDU without BITPIX"))?;
    let naxis = int_of(cards, "NAXIS").unwrap_or(0);
    if naxis <= 0 {
        return Ok(0);
    }
    let mut n: u64 = 1;
    for k in 1..=naxis {
        let v = int_of(cards, &format!("NAXIS{k}"))
            .ok_or_else(|| malformed(format!("NAXIS{k} missing")))?;
        n = n
            .checked_mul(v.max(0) as u64)
            .ok_or_else(|| malformed("NAXISn overflow"))?;
    }
    let pcount = int_of(cards, "PCOUNT").unwrap_or(0).max(0) as u64;
    let gcount = int_of(cards, "GCOUNT").unwrap_or(1).max(1) as u64;
    let bytes = (bitpix.unsigned_abs() / 8) * gcount * (pcount + n);
    Ok(bytes.div_ceil(BLOCK_SIZE as u64) * BLOCK_SIZE as u64)
}

/// Every HDU of the file, plus where the last one ends and the file length.
/// Bytes after the last HDU that do not start an `XTENSION` header are
/// "trailing" and copied verbatim by the patcher.
fn read_layouts(f: &mut File) -> Result<(Vec<HduLayout>, u64, u64), FitsWriteError> {
    let file_len = f.metadata()?.len();
    let mut layouts: Vec<HduLayout> = Vec::new();
    let mut offset = 0u64;
    while offset < file_len {
        f.seek(SeekFrom::Start(offset))?;
        let mut cards = Vec::new();
        let mut blocks = 0usize;
        let mut found_end = false;
        while !found_end {
            let mut block = [0u8; BLOCK_SIZE];
            if f.read_exact(&mut block).is_err() {
                if layouts.is_empty() {
                    return Err(malformed("truncated primary header"));
                }
                break;
            }
            blocks += 1;
            for chunk in block.chunks_exact(CARD_SIZE) {
                let mut rec = [0u8; CARD_SIZE];
                rec.copy_from_slice(chunk);
                if keyword_of(&rec) == "END" {
                    found_end = true;
                    break;
                }
                cards.push(rec);
            }
            if blocks > MAX_HEADER_BLOCKS {
                return Err(malformed("no END card within 64 header blocks"));
            }
        }
        let first = cards.first().map(keyword_of);
        if layouts.is_empty() {
            if first != Some("SIMPLE") {
                return Err(malformed("not a FITS file (no SIMPLE card)"));
            }
            if !found_end {
                return Err(malformed("primary header has no END card"));
            }
        } else if !found_end || first != Some("XTENSION") {
            break; // trailing bytes, not an HDU
        }
        let data_offset = offset + (blocks * BLOCK_SIZE) as u64;
        let data_len_padded = data_len_padded(&cards)?;
        layouts.push(HduLayout {
            cards,
            header_blocks: blocks,
            data_offset,
            data_len_padded,
        });
        offset = data_offset + data_len_padded;
    }
    Ok((layouts, offset.min(file_len), file_len))
}

/// The HDU holding the image: the primary when it has axes, else the first
/// tile-compressed (`BINTABLE` + `ZIMAGE = T`) or `IMAGE` extension with axes.
fn image_hdu(layouts: &[HduLayout]) -> usize {
    let naxis = |h: &HduLayout| int_of(&h.cards, "NAXIS").unwrap_or(0);
    if layouts.first().is_some_and(|h| naxis(h) > 0) {
        return 0;
    }
    layouts
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, h)| {
            let x = value_of(&h.cards, "XTENSION").unwrap_or_default();
            let zimage = value_of(&h.cards, "ZIMAGE").is_some_and(|v| v == "T");
            (x == "BINTABLE" && zimage) || (x == "IMAGE" && naxis(h) > 0)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn apply_edits(
    cards: &[Record],
    edits: &[CardEdit],
    is_image: bool,
) -> Result<Vec<Record>, FitsWriteError> {
    let mut out = cards.to_vec();
    for edit in edits {
        match edit {
            CardEdit::Set(card) | CardEdit::SetIfPresent(card) => {
                let present = spans_of(&out, &card.keyword);
                let insert_ok = matches!(edit, CardEdit::Set(_)) && is_image;
                if present.is_empty() && !insert_ok {
                    continue;
                }
                let records = format_card(card)?;
                match present.first() {
                    Some(&(start, len)) => {
                        for &(s, l) in present.iter().skip(1).rev() {
                            out.drain(s..s + l);
                        }
                        out.splice(start..start + len, records);
                    }
                    None => out.extend(records),
                }
            }
            CardEdit::Remove(key) => {
                for &(s, l) in spans_of(&out, key).iter().rev() {
                    out.drain(s..s + l);
                }
            }
        }
    }
    Ok(out)
}

fn render_header(cards: &[Record]) -> Vec<u8> {
    let mut out = Vec::with_capacity((cards.len() + 1) * CARD_SIZE);
    for c in cards {
        out.extend_from_slice(c);
    }
    let mut end = [b' '; CARD_SIZE];
    end[..3].copy_from_slice(b"END");
    out.extend_from_slice(&end);
    let len = out.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
    out.resize(len, b' ');
    out
}

/// Refuse a patch whose destination IS the source (a landed replica must
/// never be rewritten in place). Only resolvable when `dest` exists.
pub(crate) fn refuse_same_file(src: &Path, dest: &Path) -> Result<(), FitsWriteError> {
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(src), std::fs::canonicalize(dest)) {
        if a == b {
            return Err(malformed(format!(
                "refusing to patch {} onto itself",
                src.display()
            )));
        }
    }
    Ok(())
}

/// A unique temp path beside `dest` (pid + sequence, as the writers use).
pub(crate) fn sibling_tmp(dest: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-patch.{}.{}", std::process::id(), seq));
    dest.with_file_name(name)
}

/// Rename `tmp` over `dest`; on failure remove `tmp` and return the rename error.
pub(crate) fn commit_tmp(tmp: &Path, dest: &Path) -> Result<(), FitsWriteError> {
    match super::writer::rename_replace(tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Err(rm) = std::fs::remove_file(tmp) {
                if rm.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %tmp.display(), error = %rm, "temp file not removed");
                }
            }
            Err(e.into())
        }
    }
}

/// Remove a temp file after a failed write; a failed removal is logged.
pub(crate) fn discard_tmp(tmp: &Path, what: &str) {
    if let Err(rm) = std::fs::remove_file(tmp) {
        if rm.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %tmp.display(), error = %rm, what, "temp file not removed");
        }
    }
}

/// Copy `src` to `dest` with `edits` applied to the headers.
pub fn patch_cards(
    src: &Path,
    dest: &Path,
    edits: &[CardEdit],
) -> Result<PatchReport, FitsWriteError> {
    refuse_same_file(src, dest)?;
    let mut f = File::open(src)?;
    let (layouts, hdus_end, file_len) = read_layouts(&mut f)?;
    let image = image_hdu(&layouts);
    let tmp = sibling_tmp(dest);
    let written = (|| -> Result<usize, FitsWriteError> {
        let mut w = BufWriter::new(File::create(&tmp)?);
        let mut grown = 0usize;
        for (i, h) in layouts.iter().enumerate() {
            let cards = apply_edits(&h.cards, edits, i == image)?;
            let header = render_header(&cards);
            grown += (header.len() / BLOCK_SIZE).saturating_sub(h.header_blocks);
            w.write_all(&header)?;
            f.seek(SeekFrom::Start(h.data_offset))?;
            let n = h
                .data_len_padded
                .min(file_len.saturating_sub(h.data_offset));
            std::io::copy(&mut Read::by_ref(&mut f).take(n), &mut w)?;
        }
        if hdus_end < file_len {
            f.seek(SeekFrom::Start(hdus_end))?;
            std::io::copy(&mut f, &mut w)?;
        }
        w.flush()?;
        Ok(grown)
    })();
    match written {
        Ok(grown_blocks) => {
            commit_tmp(&tmp, dest)?;
            Ok(PatchReport {
                hdus: layouts.len(),
                image_hdu: image,
                grown_blocks,
            })
        }
        Err(e) => {
            discard_tmp(&tmp, "header patch failed");
            Err(e)
        }
    }
}

/// The image HDU's value of `key`, else the primary's.
pub fn read_image_keyword(path: &Path, key: &str) -> Result<Option<String>, FitsWriteError> {
    let mut f = File::open(path)?;
    let (layouts, _, _) = read_layouts(&mut f)?;
    let image = image_hdu(&layouts);
    Ok(value_of(&layouts[image].cards, key).or_else(|| value_of(&layouts[0].cards, key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::{write_fits_f32, CardValue};

    fn rec(s: &str) -> [u8; CARD_SIZE] {
        let mut r = [b' '; CARD_SIZE];
        r[..s.len()].copy_from_slice(s.as_bytes());
        r
    }
    fn kv(k: &str, v: &str) -> String {
        format!("{k:<8}= {v:>20}")
    }
    fn ks(k: &str, v: &str) -> String {
        format!("{k:<8}= '{v:<8}'")
    }
    /// One HDU: the cards, END, blank-padded to a block; the data zero-padded.
    fn hdu(cards: &[String], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in cards {
            out.extend_from_slice(&rec(c));
        }
        out.extend_from_slice(&rec("END"));
        out.resize(out.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, b' ');
        let start = out.len();
        out.extend_from_slice(data);
        out.resize(start + data.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, 0);
        out
    }
    fn filter_card(v: &str) -> Card {
        Card::new("FILTER", CardValue::Str(v.into())).unwrap()
    }
    /// The bytes of each HDU's data unit, in order.
    fn data_units(path: &Path) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(path).unwrap();
        let mut f = File::open(path).unwrap();
        let (layouts, _, _) = read_layouts(&mut f).unwrap();
        layouts
            .iter()
            .map(|h| {
                let s = h.data_offset as usize;
                bytes[s..(s + h.data_len_padded as usize).min(bytes.len())].to_vec()
            })
            .collect()
    }

    #[test]
    fn replaces_filter_in_place_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        let data: Vec<f32> = (0..16).map(|v| v as f32).collect();
        write_fits_f32(&src, 4, 4, 1, &data, &[filter_card("Red")]).unwrap();
        let before = std::fs::read(&src).unwrap();
        let dest = dir.path().join("b.fits");
        let card = filter_card("R").with_comment("was 'Red'");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(card)]).unwrap();
        assert_eq!(report.grown_blocks, 0);
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        assert_eq!(data_units(&src), data_units(&dest));
        assert_eq!(std::fs::read(&src).unwrap(), before, "src is never written");
        assert_eq!(
            std::fs::metadata(&src).unwrap().len(),
            std::fs::metadata(&dest).unwrap().len()
        );
    }

    #[test]
    fn inserts_a_missing_card_before_end() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.fits");
        patch_cards(&src, &dest, &[CardEdit::Set(filter_card("Ha"))]).unwrap();
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("Ha")
        );
    }

    #[test]
    fn grows_by_one_block_when_the_end_block_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("full.fits");
        // 35 cards + END = 36 records = exactly one block.
        let mut cards = vec![
            kv("SIMPLE", "T"),
            kv("BITPIX", "8"),
            kv("NAXIS", "1"),
            kv("NAXIS1", "4"),
        ];
        for i in 0..31 {
            cards.push(kv(&format!("PAD{i:02}"), "1"));
        }
        assert_eq!(cards.len(), 35);
        std::fs::write(&src, hdu(&cards, &[1, 2, 3, 4])).unwrap();
        let dest = dir.path().join("b.fits");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(filter_card("L"))]).unwrap();
        assert_eq!(report.grown_blocks, 1);
        assert_eq!(
            std::fs::metadata(&dest).unwrap().len(),
            std::fs::metadata(&src).unwrap().len() + 2880
        );
        assert_eq!(data_units(&src), data_units(&dest));
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("L")
        );
    }

    #[test]
    fn drops_later_duplicates_and_honours_set_if_present_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("dup.fits");
        let cards = vec![
            kv("SIMPLE", "T"),
            kv("BITPIX", "8"),
            kv("NAXIS", "1"),
            kv("NAXIS1", "2"),
            ks("FILTER", "Red"),
            ks("FILTER", "Rouge"),
            kv("CHECKSUM", "1"),
        ];
        std::fs::write(&src, hdu(&cards, &[7, 7])).unwrap();
        let dest = dir.path().join("b.fits");
        let ath = Card::new("ATH_FILT", CardValue::Str("R".into())).unwrap();
        patch_cards(
            &src,
            &dest,
            &[
                CardEdit::Set(filter_card("R")),
                CardEdit::SetIfPresent(ath),
                CardEdit::Remove("CHECKSUM".into()),
            ],
        )
        .unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&dest).unwrap()[..2880]).into_owned();
        assert_eq!(text.matches("FILTER  =").count(), 1);
        assert!(!text.contains("ATH_FILT"), "SetIfPresent never inserts");
        assert!(!text.contains("CHECKSUM"));
    }

    #[test]
    fn patches_the_compressed_image_extension_and_the_primary_and_streams_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("mef.fits");
        let primary = hdu(
            &[
                kv("SIMPLE", "T"),
                kv("BITPIX", "8"),
                kv("NAXIS", "0"),
                kv("EXTEND", "T"),
                ks("FILTER", "Red"),
            ],
            &[],
        );
        let comp = hdu(
            &[
                ks("XTENSION", "BINTABLE"),
                kv("BITPIX", "8"),
                kv("NAXIS", "2"),
                kv("NAXIS1", "4"),
                kv("NAXIS2", "2"),
                kv("PCOUNT", "0"),
                kv("GCOUNT", "1"),
                kv("TFIELDS", "1"),
                kv("ZIMAGE", "T"),
                ks("FILTER", "Red"),
            ],
            &[1, 2, 3, 4, 5, 6, 7, 8],
        );
        let other = hdu(
            &[
                ks("XTENSION", "IMAGE"),
                kv("BITPIX", "8"),
                kv("NAXIS", "1"),
                kv("NAXIS1", "3"),
                kv("PCOUNT", "0"),
                kv("GCOUNT", "1"),
            ],
            &[9, 9, 9],
        );
        let mut bytes = primary.clone();
        bytes.extend_from_slice(&comp);
        bytes.extend_from_slice(&other);
        bytes.extend_from_slice(b"trailing bytes");
        std::fs::write(&src, &bytes).unwrap();
        let dest = dir.path().join("b.fits");
        let report = patch_cards(&src, &dest, &[CardEdit::Set(filter_card("R"))]).unwrap();
        assert_eq!((report.hdus, report.image_hdu), (3, 1));
        let out = std::fs::read(&dest).unwrap();
        assert!(
            String::from_utf8_lossy(&out[..2880]).contains("FILTER  = 'R"),
            "primary carried FILTER, so it is set"
        );
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        let other_at = primary.len() + comp.len();
        assert_eq!(
            &out[other_at..other_at + other.len()],
            &other[..],
            "an untouched HDU is byte-identical"
        );
        assert!(out.ends_with(b"trailing bytes"));
        assert_eq!(data_units(&src), data_units(&dest));
    }

    #[test]
    fn a_long_value_uses_continue_and_reads_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let dest = dir.path().join("b.fits");
        let long = "N".repeat(90);
        patch_cards(&src, &dest, &[CardEdit::Set(filter_card(&long))]).unwrap();
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some(long.as_str())
        );
    }

    #[test]
    fn refuses_to_patch_a_file_onto_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[]).unwrap();
        let err = patch_cards(&src, &src, &[CardEdit::Set(filter_card("R"))]).unwrap_err();
        assert!(err.to_string().contains("onto itself"), "{err}");
    }

    #[test]
    fn commit_tmp_to_a_missing_parent_fails_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("x.tmp");
        std::fs::write(&tmp, b"data").unwrap();
        let dest = dir.path().join("no_such_dir").join("out.fits");
        assert!(commit_tmp(&tmp, &dest).is_err());
        assert!(!tmp.exists());
    }

    #[test]
    fn a_file_without_simple_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.fits");
        std::fs::write(&src, vec![b'x'; 2880]).unwrap();
        assert!(patch_cards(&src, &dir.path().join("y.fits"), &[]).is_err());
    }
}
