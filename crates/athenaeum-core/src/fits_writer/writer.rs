//! FITS primary-HDU file serialization: BITPIX=-32 (IEEE single-precision float),
//! structural cards (SIMPLE/BITPIX/NAXIS/NAXISn) owned by the writer, user cards
//! passed through unchanged, terminated by END, header + data padded to 2880-byte
//! blocks. No BZERO/BSCALE (float BITPIX doesn't need them); data written
//! big-endian, plane-major for channels=3 (all R, then G, then B).

use std::io::Write;
use std::path::Path;

use super::card::{format_card, Card, CardValue, FitsWriteError, BLOCK_SIZE, CARD_SIZE};
use super::Durability;

/// Format one card and append its 80-byte record(s) to `records`.
fn push(records: &mut Vec<[u8; CARD_SIZE]>, c: Card) -> Result<(), FitsWriteError> {
    records.extend(format_card(&c)?);
    Ok(())
}

/// Validate channel count and data length before any I/O happens. Shared by
/// `write_fits_f32` and `write_fits_f32_to` so a bad call never touches disk.
/// M4d Task 2: `pub(super)` — the sibling XISF writer validates the same
/// geometry contract (1 or 3 channels, non-zero dimensions, matching data
/// length) through this one function rather than a second copy of it.
pub(super) fn validate(
    width: usize,
    height: usize,
    channels: usize,
    data_len: usize,
) -> Result<(), FitsWriteError> {
    if channels != 1 && channels != 3 {
        return Err(FitsWriteError::BadChannels(channels));
    }
    if width == 0 || height == 0 {
        return Err(FitsWriteError::BadDimensions(format!("{width}x{height}")));
    }
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(channels))
        .ok_or_else(|| {
            FitsWriteError::BadDimensions(format!("{width}x{height}x{channels} overflows"))
        })?;
    if data_len != expected {
        return Err(FitsWriteError::DataSizeMismatch {
            expected,
            got: data_len,
        });
    }
    Ok(())
}

/// `fs::rename` replaces an existing destination on every platform (Windows:
/// MOVEFILE_REPLACE_EXISTING), but on Windows it fails with a sharing
/// violation while another process (AV real-time scan, indexer, a stacker
/// with the master open) holds the destination without FILE_SHARE_DELETE —
/// POSIX rename never does. Bounded retry: 5 attempts, 50→400 ms backoff.
#[cfg(windows)]
pub(crate) fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let mut delay = std::time::Duration::from_millis(50);
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..5 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_SHARING_VIOLATION) | Some(ERROR_ACCESS_DENIED)
                ) =>
            {
                last = Some(e);
                // Only back off when another attempt actually follows —
                // sleeping after the last one is pure wasted latency.
                if attempt < 4 {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("loop ran at least once"))
}

#[cfg(not(windows))]
pub(crate) fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Write a FITS file at `path`, replacing any existing file only after the write
/// fully succeeds. Validates first (so a bad call never touches `path`), then
/// writes to a sibling temp file and atomically renames it into place — a
/// pre-existing good file at `path` is never truncated by a failed write.
/// Always [`Durability::Durable`] — see [`write_fits_f32_with`] for a caller
/// that can afford to skip the fsync.
pub fn write_fits_f32(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    write_fits_f32_with(
        path,
        width,
        height,
        channels,
        data,
        cards,
        Durability::Durable,
    )
}

/// [`write_fits_f32`] with the durability as a parameter (perf tier 1 Task
/// 6). `Durability::Durable` `sync_all`s the temp file before the rename —
/// unchanged behavior for every master/export/send writer. `Durability::
/// Volatile` skips the fsync: the temp-file + rename write is still atomic
/// (a reader never observes a partial file at `path`), but a crash between
/// the flush and the rename can lose the write entirely — acceptable only
/// for the stacking run's own calibrated intermediates, which a cache-row
/// hash + stat make a cache miss rather than a wrong answer when lost.
pub fn write_fits_f32_with(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
    durability: Durability,
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
        if durability == Durability::Durable {
            // Power-loss durability: data must be on disk before the rename
            // makes the file visible under its final name.
            w.get_ref().sync_all()?;
        }
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

/// Header records + the terminating 2880-byte pad — shared by
/// [`write_fits_f32_to`] (which then writes the whole `data` slice in one
/// go) and [`write_fits_f32_streaming_to`] (fix round 2, ruling R-TA-6,
/// required item: one shared header writer so the two data-writing shapes
/// can never drift apart on what the header itself says).
fn write_header<W: Write>(
    w: &mut W,
    width: usize,
    height: usize,
    channels: usize,
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    let mut records: Vec<[u8; CARD_SIZE]> = Vec::new();
    push(
        &mut records,
        Card::structural(
            "SIMPLE",
            CardValue::Logical(true),
            "conforms to FITS standard",
        ),
    )?;
    push(
        &mut records,
        Card::structural(
            "BITPIX",
            CardValue::Integer(-32),
            "IEEE single precision floating point",
        ),
    )?;
    let naxis: i64 = if channels == 3 { 3 } else { 2 };
    push(
        &mut records,
        Card::structural("NAXIS", CardValue::Integer(naxis), "number of data axes"),
    )?;
    push(
        &mut records,
        Card::structural("NAXIS1", CardValue::Integer(width as i64), "width"),
    )?;
    push(
        &mut records,
        Card::structural("NAXIS2", CardValue::Integer(height as i64), "height"),
    )?;
    if channels == 3 {
        push(
            &mut records,
            Card::structural("NAXIS3", CardValue::Integer(3), "color planes"),
        )?;
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
    Ok(())
}

/// Writes `data` (one big-endian f32 chunk at a time) and the trailing
/// 2880-byte pad. Shared by [`write_fits_f32_to`] and
/// [`write_fits_f32_streaming_to`] (one plane at a time) — the SAME
/// chunking loop either way, so neither shape can byte-drift from the
/// other. `data_bytes` is the TOTAL image size (`width*height*channels*4`)
/// even when this call only wrote one plane's worth — the pad is written
/// once, by the LAST caller, via `is_last_chunk`.
fn write_data_chunk<W: Write>(
    w: &mut W,
    data: &[f32],
    total_data_bytes: usize,
    is_last_chunk: bool,
) -> Result<(), FitsWriteError> {
    let mut buf = Vec::with_capacity(8192 * 4);
    for v in data {
        buf.extend_from_slice(&v.to_be_bytes());
        if buf.len() >= 8192 * 4 {
            w.write_all(&buf)?;
            buf.clear();
        }
    }
    w.write_all(&buf)?;
    if is_last_chunk {
        let dpad = (BLOCK_SIZE - total_data_bytes % BLOCK_SIZE) % BLOCK_SIZE;
        w.write_all(&vec![0u8; dpad])?;
    }
    Ok(())
}

/// [`write_fits_f32_streaming_to`] wrapped in the SAME tmp-file + atomic-
/// rename shell [`write_fits_f32_with`] uses (fix round 2, ruling R-TA-6,
/// required item) — validates first, writes to a sibling temp file one
/// plane at a time, flushes, optionally `sync_all`s per `durability`, then
/// renames into place; a failed write never touches an existing good file
/// at `path`. `plane`'s error type is [`FitsWriteError`] so this module
/// stays free of a caller's own error type — `stacking::register::writer`
/// maps its `anyhow::Error` (a `PlaneReader` read failure) into
/// `FitsWriteError::Io` at the boundary.
pub fn write_fits_f32_streaming_with(
    path: &Path,
    width: usize,
    height: usize,
    channels: usize,
    cards: &[Card],
    durability: Durability,
    mut plane: impl FnMut(usize) -> Result<Vec<f32>, FitsWriteError>,
) -> Result<(), FitsWriteError> {
    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        path.with_extension(format!("fits.tmp.{}.{}", std::process::id(), seq))
    };
    let write_result = (|| -> Result<(), FitsWriteError> {
        let f = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        write_fits_f32_streaming_to(&mut w, width, height, channels, cards, &mut plane)?;
        w.flush()?;
        if durability == Durability::Durable {
            w.get_ref().sync_all()?;
        }
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

pub fn write_fits_f32_to<W: Write>(
    mut w: W,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[Card],
) -> Result<(), FitsWriteError> {
    validate(width, height, channels, data.len())?;
    write_header(&mut w, width, height, channels, cards)?;
    write_data_chunk(&mut w, data, data.len() * 4, true)
}

/// Streaming per-plane counterpart of [`write_fits_f32_to`] (fix round 2,
/// ruling R-TA-6, required item — the plane-at-a-time registered-frame
/// write): the header is IDENTICAL (`write_header`, shared), but instead
/// of one `data: &[f32]` slice already holding every plane, `plane` is
/// called once per channel (`0..channels`, the SAME FITS plane-major order
/// — R, then G, then B for a 3-channel image — `write_fits_f32_to` writes
/// a pre-assembled buffer in) and must return exactly `width * height`
/// values for THAT plane. Nothing here holds more than one plane's bytes
/// resident at once, so a caller that also PRODUCES its plane data lazily
/// (warp one plane, write it, drop it, warp the next) never has to
/// assemble the whole image in memory first — the reason this exists.
/// Because it calls the exact same `write_header`/`write_data_chunk` a
/// pre-assembled write does, over the exact same per-plane byte ranges,
/// the resulting file is byte-identical to what [`write_fits_f32_to`]
/// would write given the same planes concatenated into one slice —
/// pinned by `stacking::register::writer`'s own tests via `cmp`, not
/// re-proven here.
pub fn write_fits_f32_streaming_to<W: Write>(
    mut w: W,
    width: usize,
    height: usize,
    channels: usize,
    cards: &[Card],
    mut plane: impl FnMut(usize) -> Result<Vec<f32>, FitsWriteError>,
) -> Result<(), FitsWriteError> {
    if channels != 1 && channels != 3 {
        return Err(FitsWriteError::BadChannels(channels));
    }
    if width == 0 || height == 0 {
        return Err(FitsWriteError::BadDimensions(format!("{width}x{height}")));
    }
    let plane_len = width
        .checked_mul(height)
        .ok_or_else(|| FitsWriteError::BadDimensions(format!("{width}x{height} overflows")))?;
    let total_data_bytes = plane_len
        .checked_mul(channels)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| {
            FitsWriteError::BadDimensions(format!("{width}x{height}x{channels} overflows"))
        })?;

    write_header(&mut w, width, height, channels, cards)?;
    for p in 0..channels {
        let data = plane(p)?;
        if data.len() != plane_len {
            return Err(FitsWriteError::DataSizeMismatch {
                expected: plane_len,
                got: data.len(),
            });
        }
        write_data_chunk(&mut w, &data, total_data_bytes, p + 1 == channels)?;
    }
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
        let evil = Card {
            keyword: "BAD KEY!".into(),
            value: Some(CardValue::Integer(1)),
            comment: None,
            text: None,
            structural: false,
        };
        let r = crate::fits_writer::card::format_card(&evil);
        assert!(r.is_err(), "format_card must re-validate keywords: {r:?}");
        let reserved = Card {
            keyword: "NAXIS1".into(),
            value: Some(CardValue::Integer(1)),
            comment: None,
            text: None,
            structural: false,
        };
        assert!(crate::fits_writer::card::format_card(&reserved).is_err());
        // Reserved keywords must fail closed even when hand-built to mimic the
        // writer's own structural cards — only the crate-private `structural`
        // capability flag (Card::structural) exempts a card, never its name.
        for kw in ["SIMPLE", "BITPIX", "END"] {
            let fake = Card {
                keyword: kw.into(),
                value: Some(CardValue::Integer(1)),
                comment: None,
                text: None,
                structural: false,
            };
            let r = crate::fits_writer::card::format_card(&fake);
            assert!(r.is_err(), "hand-built {kw} card must be rejected: {r:?}");
        }
        // A comment is caller-settable and must not act as a trust signal.
        let fake_naxis = Card {
            keyword: "NAXIS1".into(),
            value: Some(CardValue::Integer(1)),
            comment: Some("x".into()),
            text: None,
            structural: false,
        };
        let r = crate::fits_writer::card::format_card(&fake_naxis);
        assert!(
            r.is_err(),
            "NAXIS1 with a comment must still be rejected: {r:?}"
        );
    }

    #[test]
    fn text_card_with_no_value_is_error_not_panic() {
        // value: None + text: None used to hit `expect("value card")`.
        let broken = Card {
            keyword: "GAIN".into(),
            value: None,
            comment: None,
            text: None,
            structural: false,
        };
        assert!(crate::fits_writer::card::format_card(&broken).is_err());
    }

    #[test]
    fn volatile_write_is_still_atomic_and_identical() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<f32> = (0..64 * 48).map(|i| i as f32 * 0.5).collect();
        let a = dir.path().join("durable.fits");
        let b = dir.path().join("volatile.fits");
        write_fits_f32_with(&a, 64, 48, 1, &data, &[], Durability::Durable).unwrap();
        write_fits_f32_with(&b, 64, 48, 1, &data, &[], Durability::Volatile).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        // No tmp file left behind either way.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    /// Fix round 2, ruling R-TA-6, required item: the plane-at-a-time
    /// streaming writer must produce BYTE-IDENTICAL output to the
    /// all-at-once writer, for a 1-channel and a 3-channel image, over
    /// both durability modes. `stacking::register::writer`'s own tests
    /// re-verify this at the `write_registered_frame` level (a synthetic
    /// fixture AND a real OSC frame, via `cmp`) — this is the foundational
    /// pin at the primitive level, mono and OSC alike.
    #[test]
    fn streaming_write_matches_the_all_at_once_writer_byte_for_byte() {
        for channels in [1usize, 3usize] {
            let (w, h) = (37usize, 29usize); // an odd size: no accidental block alignment
            let plane_len = w * h;
            let planes: Vec<Vec<f32>> = (0..channels)
                .map(|c| {
                    (0..plane_len)
                        .map(|i| (c * 1000 + i) as f32 * 0.125 - 3.0)
                        .collect()
                })
                .collect();
            let all: Vec<f32> = planes.iter().flatten().copied().collect();
            let cards = vec![Card::new("EXPTIME", CardValue::Real(30.0)).unwrap()];

            let dir = tempfile::tempdir().unwrap();
            for durability in [Durability::Durable, Durability::Volatile] {
                let whole = dir
                    .path()
                    .join(format!("whole_{channels}_{durability:?}.fits"));
                let streamed = dir
                    .path()
                    .join(format!("streamed_{channels}_{durability:?}.fits"));
                write_fits_f32_with(&whole, w, h, channels, &all, &cards, durability).unwrap();
                write_fits_f32_streaming_with(&streamed, w, h, channels, &cards, durability, |p| {
                    Ok(planes[p].clone())
                })
                .unwrap();
                assert_eq!(
                    std::fs::read(&whole).unwrap(),
                    std::fs::read(&streamed).unwrap(),
                    "channels={channels} durability={durability:?}: streaming and \
                     all-at-once writers disagree"
                );
            }
        }
    }
}
