//! Byte-level FITS header stamping: copy a simple single-HDU FITS file,
//! inserting ONE extra card before END. Our own `write_fits_f32` outputs are
//! the only intended inputs (single HDU, 2880-byte header blocks). No pixel
//! decode — the data region is streamed verbatim.
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use super::card::{format_card, Card, FitsWriteError, CARD_SIZE};

const BLOCK: usize = 2880;

pub fn stamp_extra_card(src: &Path, dest: &Path, card: &Card) -> Result<(), FitsWriteError> {
    let mut reader = BufReader::new(File::open(src).map_err(FitsWriteError::Io)?);
    // Read header blocks until the one containing END.
    let mut header: Vec<u8> = Vec::with_capacity(BLOCK * 2);
    let mut end_at: Option<usize> = None;
    while end_at.is_none() {
        let mut block = [0u8; BLOCK];
        reader.read_exact(&mut block).map_err(FitsWriteError::Io)?;
        let base = header.len();
        header.extend_from_slice(&block);
        for i in (0..BLOCK).step_by(CARD_SIZE) {
            if &block[i..i + 8] == b"END     " {
                end_at = Some(base + i);
                break;
            }
        }
        if header.len() > BLOCK * 64 {
            return Err(FitsWriteError::Malformed(
                "no END card in the first 64 header blocks".into(),
            ));
        }
    }
    let end_at = end_at.expect("loop exits only with END found");
    let new_records = format_card(card)?; // CONTINUE-capable: may be several 80-byte records
    let needed = new_records.len() * CARD_SIZE;
    // Insert before END; header must stay 2880-aligned (grow by whole blocks as needed).
    let used_after = end_at + CARD_SIZE + needed; // cards incl. END after insertion
    let new_header_len = used_after.div_ceil(BLOCK) * BLOCK;
    let mut out: Vec<u8> = Vec::with_capacity(new_header_len);
    out.extend_from_slice(&header[..end_at]);
    for rec in &new_records {
        out.extend_from_slice(rec);
    }
    out.extend_from_slice(b"END");
    out.resize(out.len() + (CARD_SIZE - 3), b' '); // pad END record to 80
    out.resize(new_header_len, b' '); // pad header to block boundary
    let tmp = dest.with_extension("tmp-stamp");
    {
        let mut w = BufWriter::new(File::create(&tmp).map_err(FitsWriteError::Io)?);
        w.write_all(&out).map_err(FitsWriteError::Io)?;
        std::io::copy(&mut reader, &mut w).map_err(FitsWriteError::Io)?; // data region verbatim
        w.flush().map_err(FitsWriteError::Io)?;
    }
    std::fs::rename(&tmp, dest).map_err(FitsWriteError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;

    #[test]
    fn stamped_copy_parses_with_new_card_and_identical_data() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.fits");
        let data: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let cards = vec![Card::new("ATH_TEST", crate::fits_writer::CardValue::Integer(7)).unwrap()];
        write_fits_f32(&src, 4, 4, 1, &data, &cards).unwrap();
        let dest = dir.path().join("b.fits");
        stamp_extra_card(
            &src,
            &dest,
            &Card::new("ATH_PRJ", crate::fits_writer::CardValue::Str("proj-uuid".into())).unwrap(),
        )
        .unwrap();
        let src_bytes = std::fs::read(&src).unwrap();
        let dest_bytes = std::fs::read(&dest).unwrap();
        // data region identical — compare the first 64 data bytes right AFTER each
        // header (the tail is zero padding on both files, a vacuous compare):
        let data_start = |b: &[u8]| {
            (0..b.len())
                .step_by(2880)
                .find(|&o| b[o..].chunks(80).take(36).any(|c| c.starts_with(b"END ")))
                .map(|o| o + 2880)
                .unwrap()
        };
        let (s0, d0) = (data_start(&src_bytes), data_start(&dest_bytes));
        assert_eq!(&src_bytes[s0..s0 + 64], &dest_bytes[d0..d0 + 64]);
        // stamped header contains both keywords:
        let head = String::from_utf8_lossy(&dest_bytes[..dest_bytes.len() - 16 * 4]);
        assert!(head.contains("ATH_PRJ") && head.contains("ATH_TEST"));
        // header stays block-aligned:
        assert_eq!(dest_bytes.len() % 2880, 0);
    }
}
