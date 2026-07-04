//! FITS primary-HDU file serialization: BITPIX=-32 (IEEE single-precision float),
//! structural cards (SIMPLE/BITPIX/NAXIS/NAXISn) owned by the writer, user cards
//! passed through unchanged, terminated by END, header + data padded to 2880-byte
//! blocks. No BZERO/BSCALE (float BITPIX doesn't need them); data written
//! big-endian, plane-major for channels=3 (all R, then G, then B).

use std::io::Write;
use std::path::Path;

use super::card::{format_card, Card, CardValue, FitsWriteError, BLOCK_SIZE, CARD_SIZE};

/// Format one card and append its 80-byte record(s) to `records`.
fn push(records: &mut Vec<[u8; CARD_SIZE]>, c: Card) -> Result<(), FitsWriteError> {
    records.extend(format_card(&c)?);
    Ok(())
}

/// Validate channel count and data length before any I/O happens. Shared by
/// `write_fits_f32` and `write_fits_f32_to` so a bad call never touches disk.
fn validate(width: usize, height: usize, channels: usize, data_len: usize) -> Result<(), FitsWriteError> {
    if channels != 1 && channels != 3 {
        return Err(FitsWriteError::BadChannels(channels));
    }
    if width == 0 || height == 0 {
        return Err(FitsWriteError::BadDimensions(format!("{width}x{height}")));
    }
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| FitsWriteError::BadDimensions(format!("{width}x{height}x{channels} overflows")))?;
    if data_len != expected {
        return Err(FitsWriteError::DataSizeMismatch { expected, got: data_len });
    }
    Ok(())
}

/// Write a FITS file at `path`, replacing any existing file only after the write
/// fully succeeds. Validates first (so a bad call never touches `path`), then
/// writes to a sibling temp file and atomically renames it into place — a
/// pre-existing good file at `path` is never truncated by a failed write.
pub fn write_fits_f32(
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
        path.with_extension(format!("fits.tmp.{}.{}", std::process::id(), seq))
    };
    let write_result = (|| -> Result<(), FitsWriteError> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        write_fits_f32_to(&mut w, width, height, channels, data, cards)?;
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

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn write_fits_f32_to<W: Write>(
    mut w: W,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    validate(width, height, channels, data.len())?;

    let mut records: Vec<[u8; CARD_SIZE]> = Vec::new();
    push(&mut records, Card::structural("SIMPLE", CardValue::Logical(true), "conforms to FITS standard"))?;
    push(&mut records, Card::structural("BITPIX", CardValue::Integer(-32), "IEEE single precision floating point"))?;
    let naxis: i64 = if channels == 3 { 3 } else { 2 };
    push(&mut records, Card::structural("NAXIS", CardValue::Integer(naxis), "number of data axes"))?;
    push(&mut records, Card::structural("NAXIS1", CardValue::Integer(width as i64), "width"))?;
    push(&mut records, Card::structural("NAXIS2", CardValue::Integer(height as i64), "height"))?;
    if channels == 3 {
        push(&mut records, Card::structural("NAXIS3", CardValue::Integer(3), "color planes"))?;
    }
    for c in cards {
        records.extend(format_card(c)?);
    }
    // END card
    let mut end = [b' '; CARD_SIZE];
    end[..3].copy_from_slice(b"END");
    records.push(end);

    for r in &records {
        w.write_all(r)?;
    }
    // pad header to 2880 with ASCII spaces
    let header_bytes = records.len() * CARD_SIZE;
    let pad = (BLOCK_SIZE - header_bytes % BLOCK_SIZE) % BLOCK_SIZE;
    w.write_all(&vec![b' '; pad])?;

    // data: big-endian f32, plane-major
    let mut buf = Vec::with_capacity(8192 * 4);
    for v in data {
        buf.extend_from_slice(&v.to_be_bytes());
        if buf.len() >= 8192 * 4 {
            w.write_all(&buf)?;
            buf.clear();
        }
    }
    w.write_all(&buf)?;
    let data_bytes = data.len() * 4;
    let dpad = (BLOCK_SIZE - data_bytes % BLOCK_SIZE) % BLOCK_SIZE;
    w.write_all(&vec![0u8; dpad])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::card::{Card, CardValue};

    #[test]
    fn zero_dimensions_rejected() {
        let r = write_fits_f32_to(std::io::sink(), 0, 10, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
        let r = write_fits_f32_to(std::io::sink(), 10, 0, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
    }

    #[test]
    fn dimension_overflow_rejected_not_panicking() {
        // usize::MAX * 3 would overflow the expected-length multiply
        let r = write_fits_f32_to(std::io::sink(), usize::MAX, 2, 1, &[], &[]);
        assert!(matches!(r, Err(FitsWriteError::BadDimensions(_))), "{r:?}");
    }

    #[test]
    fn concurrent_same_target_writers_do_not_collide_on_tmp() {
        // Two threads writing the same path: both must succeed (last rename
        // wins) — with a fixed ".fits.tmp" suffix one thread unlinks the
        // other's tmp and rename fails with NotFound.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.fits");
        let mk = |v: f32| {
            let path = path.clone();
            std::thread::spawn(move || {
                let data = vec![v; 64 * 64];
                for _ in 0..20 {
                    write_fits_f32(&path, 64, 64, 1, &data, &[]).unwrap();
                }
            })
        };
        let (a, b) = (mk(1.0), mk(2.0));
        a.join().unwrap();
        b.join().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn bypassed_card_constructor_still_validated_at_format_time() {
        // Card fields are pub — a caller can build an invalid keyword directly.
        let evil = Card { keyword: "BAD KEY!".into(), value: Some(CardValue::Integer(1)), comment: None, text: None, structural: false };
        let r = crate::fits_writer::card::format_card(&evil);
        assert!(r.is_err(), "format_card must re-validate keywords: {r:?}");
        let reserved = Card { keyword: "NAXIS1".into(), value: Some(CardValue::Integer(1)), comment: None, text: None, structural: false };
        assert!(crate::fits_writer::card::format_card(&reserved).is_err());
        // Reserved keywords must fail closed even when hand-built to mimic the
        // writer's own structural cards — only the crate-private `structural`
        // capability flag (Card::structural) exempts a card, never its name.
        for kw in ["SIMPLE", "BITPIX", "END"] {
            let fake = Card { keyword: kw.into(), value: Some(CardValue::Integer(1)), comment: None, text: None, structural: false };
            let r = crate::fits_writer::card::format_card(&fake);
            assert!(r.is_err(), "hand-built {kw} card must be rejected: {r:?}");
        }
        // A comment is caller-settable and must not act as a trust signal.
        let fake_naxis = Card { keyword: "NAXIS1".into(), value: Some(CardValue::Integer(1)), comment: Some("x".into()), text: None, structural: false };
        let r = crate::fits_writer::card::format_card(&fake_naxis);
        assert!(r.is_err(), "NAXIS1 with a comment must still be rejected: {r:?}");
    }

    #[test]
    fn text_card_with_no_value_is_error_not_panic() {
        // value: None + text: None used to hit `expect("value card")`.
        let broken = Card { keyword: "GAIN".into(), value: None, comment: None, text: None, structural: false };
        assert!(crate::fits_writer::card::format_card(&broken).is_err());
    }
}
