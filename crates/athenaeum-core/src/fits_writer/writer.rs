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
    let expected = width * height * channels;
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

    let tmp = path.with_extension("fits.tmp");
    let write_result = (|| -> Result<(), FitsWriteError> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        write_fits_f32_to(&mut w, width, height, channels, data, cards)?;
        w.flush()?;
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
