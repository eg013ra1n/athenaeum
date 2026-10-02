//! The project export's FILTER rule (collab v3 wave 6, spec §10.2, W4):
//! every exported project file carries its frame's canonical filter in the
//! `FILTER` card. A file whose own card differs (or is absent) is exported
//! as a patched COPY; the source is only ever read.
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::fits_writer::card::format_card;
use crate::fits_writer::header_patch::{patch_cards, read_image_keyword, CardEdit};
use crate::fits_writer::xisf_patch::{patch_xisf_keywords, read_xisf_keyword, XisfEdit};
use crate::fits_writer::{Card, CardValue, FitsWriteError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFileKind {
    Fits,
    Xisf,
}

/// The container, from the first bytes (never the extension).
pub fn sniff_kind(path: &Path) -> Result<ImageFileKind> {
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .with_context(|| format!("read {}", path.display()))?;
    match &head {
        b"XISF0100" => Ok(ImageFileKind::Xisf),
        b"SIMPLE  " => Ok(ImageFileKind::Fits),
        _ => Err(anyhow!("{} is neither FITS nor XISF", path.display())),
    }
}

/// The file's own `FILTER` (the image HDU's, else the primary's; XISF: the first Image's).
pub fn read_file_filter(path: &Path) -> Result<(ImageFileKind, Option<String>)> {
    let kind = sniff_kind(path)?;
    let value = match kind {
        ImageFileKind::Fits => read_image_keyword(path, "FILTER"),
        ImageFileKind::Xisf => read_xisf_keyword(path, "FILTER"),
    }
    .map_err(|e| anyhow!("{}: {e}", path.display()))?;
    Ok((kind, value))
}

/// W4: exact, case-sensitive, after trimming; an absent card is "".
pub fn needs_rewrite(actual: Option<&str>, canonical: &str) -> bool {
    actual.map(str::trim).unwrap_or("") != canonical
}

fn was_comment(actual: Option<&str>) -> String {
    match actual.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => format!("was '{raw}'"),
        None => "was unset".to_string(),
    }
}

/// A string card whose comment is shortened (never the value) until the
/// record(s) fit 80 columns.
fn fitting_card(key: &str, value: &str, comment: &str) -> Result<Card> {
    let mut comment: Vec<char> = comment.chars().collect();
    loop {
        let text: String = comment.iter().collect();
        let mut card = Card::new(key, CardValue::Str(value.to_string()))?;
        if !text.is_empty() {
            card = card.with_comment(&text);
        }
        match format_card(&card) {
            Ok(_) => return Ok(card),
            Err(FitsWriteError::CommentTooLong(_)) if !comment.is_empty() => {
                let keep = comment.len().saturating_sub(4);
                comment.truncate(keep);
                if keep > 0 {
                    comment.extend("...".chars());
                }
            }
            Err(e) => return Err(anyhow!("{key} card: {e}")),
        }
    }
}

/// Write `dest` as a copy of `src` whose FILTER is `canonical` (ATH_FILT too
/// when present; a FITS CHECKSUM dropped — DATASUM stays valid).
pub fn write_filter_copy(
    src: &Path,
    dest: &Path,
    kind: ImageFileKind,
    canonical: &str,
    actual: Option<&str>,
) -> Result<()> {
    let comment = was_comment(actual);
    match kind {
        ImageFileKind::Fits => {
            let edits = [
                CardEdit::Set(fitting_card("FILTER", canonical, &comment)?),
                CardEdit::SetIfPresent(Card::new(
                    "ATH_FILT",
                    CardValue::Str(canonical.to_string()),
                )?),
                CardEdit::Remove("CHECKSUM".to_string()),
            ];
            patch_cards(src, dest, &edits).map(|_| ())
        }
        ImageFileKind::Xisf => {
            let quoted = format!("'{}'", canonical.replace('\'', "''"));
            let edits = [
                XisfEdit::Keyword {
                    name: "FILTER".into(),
                    value: quoted.clone(),
                    comment,
                    insert: true,
                },
                XisfEdit::Keyword {
                    name: "ATH_FILT".into(),
                    value: quoted,
                    comment: String::new(),
                    insert: false,
                },
                XisfEdit::PropertyIfPresent {
                    id: "Instrument:Filter:Name".into(),
                    value: canonical.to_string(),
                },
            ];
            patch_xisf_keywords(src, dest, &edits).map(|_| ())
        }
    }
    .map_err(|e| anyhow!("{}: {e}", src.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::header_patch::read_image_keyword;
    use crate::fits_writer::{write_fits_f32, write_xisf_f32, Card, CardValue};

    fn card(k: &str, v: &str) -> Card {
        Card::new(k, CardValue::Str(v.into())).unwrap()
    }

    #[test]
    fn the_rule_compares_exactly_after_trimming() {
        assert!(!needs_rewrite(Some("R"), "R"));
        assert!(!needs_rewrite(Some("R   "), "R"));
        assert!(needs_rewrite(Some("r"), "R"), "case-sensitive");
        assert!(needs_rewrite(Some("Red"), "R"));
        assert!(
            needs_rewrite(None, "None"),
            "an absent card differs from the canonical None"
        );
        assert!(!needs_rewrite(None, ""));
    }

    #[test]
    fn a_fits_copy_carries_the_canonical_filter_and_the_raw_in_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        write_fits_f32(
            &src,
            2,
            2,
            1,
            &[0.0; 4],
            &[
                card("FILTER", "Red"),
                card("ATH_FILT", "Red"),
                Card::new("CHECKSUM", CardValue::Str("x".into())).unwrap(),
            ],
        )
        .unwrap();
        let (kind, actual) = read_file_filter(&src).unwrap();
        assert_eq!(
            (kind, actual.as_deref()),
            (ImageFileKind::Fits, Some("Red"))
        );
        let dest = dir.path().join("b.fits");
        write_filter_copy(&src, &dest, kind, "R", actual.as_deref()).unwrap();
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        assert_eq!(
            read_image_keyword(&dest, "ATH_FILT").unwrap().as_deref(),
            Some("R")
        );
        let head = String::from_utf8_lossy(&std::fs::read(&dest).unwrap()[..2880]).into_owned();
        assert!(head.contains("/ was 'Red'"), "{head}");
        assert!(!head.contains("CHECKSUM"));
    }

    #[test]
    fn an_overlong_comment_is_truncated_never_the_value() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        let raw = "R".repeat(60);
        write_fits_f32(&src, 2, 2, 1, &[0.0; 4], &[card("FILTER", &raw)]).unwrap();
        let dest = dir.path().join("b.fits");
        write_filter_copy(
            &src,
            &dest,
            ImageFileKind::Fits,
            "Red-Long-Canonical-Name",
            Some(&raw),
        )
        .unwrap();
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("Red-Long-Canonical-Name")
        );
    }

    #[test]
    fn an_xisf_copy_carries_the_canonical_filter() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.xisf");
        write_xisf_f32(&src, 1, 1, 1, &[0.0], &[card("FILTER", "H-alpha")]).unwrap();
        let (kind, actual) = read_file_filter(&src).unwrap();
        assert_eq!(kind, ImageFileKind::Xisf);
        let dest = dir.path().join("b.xisf");
        write_filter_copy(&src, &dest, kind, "Ha", actual.as_deref()).unwrap();
        assert_eq!(read_file_filter(&dest).unwrap().1.as_deref(), Some("Ha"));
    }

    #[test]
    fn an_unknown_file_kind_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.bin");
        std::fs::write(&p, b"not an image").unwrap();
        assert!(read_file_filter(&p).is_err());
    }
}
