//! Crop to coverage (spec §5.9, plan 6b-3 D10): the rectangle a run keeps,
//! and the pure operations that apply it to a plane, a header and a file.
//!
//! The rectangle is in array pixels of the master (the grid, for a
//! project). A drizzled output uses the same rectangle `scale` times finer
//! ([`CropRect::scaled`]): every master pixel is `s × s` drizzle pixels, and
//! the drizzle WCS (`scale_plate_solve`) is defined on exactly that lattice.
//! Cropping moves only `CRPIX`: SIP terms are polynomials in
//! `(u, v) = p − CRPIX`, so they hold unchanged, and the WCS refers to array
//! coordinates, so the row order needs no special case.

use std::path::Path;

use anyhow::Context;

use crate::fits_writer::{Card, CardValue, OutputFormat, XisfBounds};
use crate::integration::plane_reader::PlaneReader;
use crate::plate_solve::storage::PlateSolveRecord;

// The rectangle, the coverage rules and `crop_plane` live in the kernel; this file keeps the two operations that
// restate a header and write a file.
pub use starloom::stacking::crop::*;

/// Re-states `CRPIX1`/`CRPIX2` in `cards` from `cropped`
/// ([`crate::stacking::grid_map::crop_plate_solve`] of the record the cards
/// were written from), as exactly the values `wcs_cards` writes for it
/// (1-based). Every other card, its comment and the presence or absence of
/// `PLTSOLVD` (a grid master has none) stay as they were. Cards with no WCS
/// are left alone.
pub fn restate_crpix(cards: &mut [Card], cropped: &PlateSolveRecord) {
    for card in cards.iter_mut() {
        let value = match card.keyword.as_str() {
            "CRPIX1" => cropped.crpix1 + 1.0,
            "CRPIX2" => cropped.crpix2 + 1.0,
            _ => continue,
        };
        card.value = Some(CardValue::Real(value));
    }
}

/// Reads the uncompressed float FITS at `src` (a scratch output), crops
/// every plane to `rect` and writes `dst` in `format` with `cards` — the
/// master writers' own call (`XisfBounds::Unit`). Refuses a rectangle that
/// does not fit the file.
pub fn crop_file(
    src: &Path,
    dst: &Path,
    rect: CropRect,
    cards: &[Card],
    format: OutputFormat,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        src != dst,
        "the crop must not overwrite its source {}",
        src.display()
    );
    let reader = PlaneReader::open(src).with_context(|| format!("opening {}", src.display()))?;
    let (width, height, channels) = (reader.width(), reader.height(), reader.channels());
    anyhow::ensure!(
        rect.fits_within(width, height),
        "crop {rect:?} does not fit {} ({width} x {height})",
        src.display()
    );
    let mut data = Vec::with_capacity(rect.width * rect.height * channels);
    for plane in 0..channels {
        let full = reader
            .read_plane(plane)
            .with_context(|| format!("reading plane {plane} of {}", src.display()))?;
        data.extend(crop_plane(&full, width, rect));
    }
    crate::fits_writer::write_image_f32(
        dst,
        rect.width,
        rect.height,
        channels,
        &data,
        cards,
        format,
        XisfBounds::Unit,
    )
    .with_context(|| format!("writing {}", dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::wcs::wcs_cards;
    use crate::stacking::grid_map::crop_plate_solve;

    /// Spec §5.9 "CRPIX shifts by the crop origin", plan 6b-3 D10: the
    /// restated header carries exactly the `CRPIX` a header written from the
    /// cropped record carries, with SIP and every other card untouched —
    /// and, on a grid header, still no `PLTSOLVD`.
    #[test]
    fn restate_crpix_moves_only_the_reference_pixel() {
        let mut rec = crate::stacking::test_fixtures::sky_frame_record(
            &crate::stacking::test_fixtures::sky_frame("a", (83.8, -5.4)),
        );
        rec.sip_order = Some(2);
        rec.sip_a_coeffs = Some("[[0.0, 0.0, 1e-6], [0.0, 2e-6, 0.0], [3e-6, 0.0, 0.0]]".into());
        rec.sip_b_coeffs = Some("[[0.0, 0.0, -1e-6], [0.0, 1e-6, 0.0], [2e-6, 0.0, 0.0]]".into());
        let original = wcs_cards(&rec).unwrap();
        let mut cards: Vec<Card> = original
            .iter()
            .filter(|c| c.keyword != "PLTSOLVD")
            .cloned()
            .collect();
        let cropped = crop_plate_solve(&rec, 40, 25);
        restate_crpix(&mut cards, &cropped);
        let expected = wcs_cards(&cropped).unwrap();
        let value = |cs: &[Card], k: &str| {
            cs.iter()
                .find(|c| c.keyword == k)
                .and_then(|c| c.value.clone())
        };
        for k in ["CRPIX1", "CRPIX2"] {
            assert_eq!(value(&cards, k), value(&expected, k), "{k}");
        }
        assert_ne!(
            value(&cards, "CRPIX1"),
            value(&original, "CRPIX1"),
            "CRPIX1 must move"
        );
        for c in &cards {
            if c.keyword != "CRPIX1" && c.keyword != "CRPIX2" {
                assert_eq!(c.value, value(&original, &c.keyword), "{} moved", c.keyword);
            }
        }
        assert!(cards.iter().all(|c| c.keyword != "PLTSOLVD"));
    }

    /// Plan 6b-3 D10: a three-plane scratch FITS crops plane by plane, to
    /// FITS and to XISF.
    #[test]
    fn crop_file_crops_every_plane() {
        let tmp = tempfile::tempdir().unwrap();
        let (w, h) = (6, 5);
        let data: Vec<f32> = (0..3 * w * h).map(|i| i as f32 * 0.01).collect();
        let src = tmp.path().join("scratch.fits");
        crate::fits_writer::write_fits_f32(&src, w, h, 3, &data, &[]).unwrap();
        let rect = CropRect {
            x0: 1,
            y0: 2,
            width: 3,
            height: 2,
        };
        let cards = vec![Card::new("BUNIT", CardValue::Str("x".into())).unwrap()];
        let fits = tmp.path().join("out.fits");
        crop_file(&src, &fits, rect, &cards, OutputFormat::Fits).unwrap();
        let reader = PlaneReader::open(&fits).unwrap();
        assert_eq!(
            (reader.width(), reader.height(), reader.channels()),
            (3, 2, 3)
        );
        for p in 0..3 {
            let expected = crop_plane(&data[p * w * h..(p + 1) * w * h], w, rect);
            assert_eq!(reader.read_plane(p).unwrap(), expected, "plane {p}");
        }
        let xisf = tmp.path().join("out.xisf");
        crop_file(&src, &xisf, rect, &cards, OutputFormat::Xisf).unwrap();
        let (meta, _) = astroimage::ImageConverter::read_raw(&xisf).unwrap();
        assert_eq!((meta.width, meta.height, meta.channels), (3, 2, 3));
        let err = crop_file(
            &src,
            &tmp.path().join("bad.fits"),
            CropRect {
                x0: 4,
                y0: 0,
                width: 3,
                height: 1,
            },
            &cards,
            OutputFormat::Fits,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("does not fit"), "{err:#}");
        let err = crop_file(&src, &src, rect, &cards, OutputFormat::Fits).unwrap_err();
        assert!(format!("{err:#}").contains("must not overwrite"), "{err:#}");
    }
}
