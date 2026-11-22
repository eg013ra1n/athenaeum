//! The LN reference's artifact writers as Athenaeum writes them: the kernel's writers (`starloom`) write `IMAGETYP` and
//! the caller's cards; these two add the scanner-skip pair `ATH_STK` / `ATH_STKV` a master light stamps, so the
//! scanner's "a file carrying `ATH_REG` / `ATH_STK` is never cataloged" rule covers the artifact.

use std::path::Path;

use crate::fits_writer::{Card, CardValue};
use crate::stacking::master_cards::ATH_STK_VERSION;
use starloom::stacking::ln::SaturationMask;

pub use starloom::stacking::ln::reference::*;

/// The scanner-skip pair `ATH_STK` / `ATH_STKV` — the identical cards (same keywords, same version constant) a master
/// light stamps — followed by `caller_cards` (e.g. `ATH_STKI`, the run id; `ATH_STKG`, the group key). The kernel puts
/// `IMAGETYP` first, so the written order is `IMAGETYP, ATH_STK, ATH_STKV, caller cards`.
fn skip_cards(caller_cards: &[Card]) -> anyhow::Result<Vec<Card>> {
    let mut cards = vec![
        Card::new("ATH_STK", CardValue::Logical(true))?
            .with_comment("stacked by Athenaeum; never cataloged"),
        Card::new("ATH_STKV", CardValue::Integer(ATH_STK_VERSION))?
            .with_comment("stacking header version"),
    ];
    cards.extend(caller_cards.iter().cloned());
    Ok(cards)
}

/// Writes the reference as a float32 FITS (planar `channels × width × height`, atomic tmp+rename) at `path`, creating
/// parent directories as needed. `cards` is the caller's own provenance; the mandatory `IMAGETYP` card and the
/// scanner-skip pair are added here, not by the caller.
pub fn write_reference(r: &LnReference, path: &Path, cards: &[Card]) -> anyhow::Result<()> {
    starloom::stacking::ln::reference::write_reference(r, path, &skip_cards(cards)?)
}

/// Writes the saturation map beside the reference (L-23): the same planar layout and the same scanner-skip and caller
/// cards [`write_reference`] gives `reference.fits`, with `IMAGETYP = 'LN Reference Saturation'`. Returns the masks, one
/// per plane — what the run keeps.
pub fn write_reference_saturation(
    maps: &mut [Vec<f32>],
    width: usize,
    height: usize,
    path: &Path,
    cards: &[Card],
    saturation: f64,
) -> anyhow::Result<Vec<SaturationMask>> {
    starloom::stacking::ln::reference::write_reference_saturation(
        maps,
        width,
        height,
        path,
        &skip_cards(cards)?,
        saturation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;


    /// The header's card keys in file order, from the first non-structural card on.
    fn header_keys(path: &Path) -> Vec<String> {
        let text = crate::fits_parser::extract_fits_header(path).unwrap();
        text.lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_string()))
            .skip_while(|k| {
                matches!(k.as_str(), "SIMPLE" | "BITPIX" | "EXTEND" | "BZERO" | "BSCALE")
                    || k.starts_with("NAXIS")
            })
            .collect()
    }

    /// Task 7 guard: the cards of an LN reference and of its saturation sidecar, written through the path the run
    /// uses (`stacking::ln::write_reference`), are `IMAGETYP`, the scanner-skip pair `ATH_STK`, `ATH_STKV`, then the
    /// caller's cards, in that order.
    #[test]
    fn athenaeum_ln_reference_cards_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let r = LnReference {
            width: 2,
            height: 2,
            planes: vec![vec![0.1f32; 4]],
            frames_used: vec![0],
        };
        let path = dir.path().join("reference.fits");
        let cards = vec![Card::new("ATH_STKI", CardValue::Integer(7)).unwrap()];
        crate::stacking::ln::write_reference(&r, &path, &cards).unwrap();
        let keys = header_keys(&path);
        assert_eq!(
            keys.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
            ["IMAGETYP", "ATH_STK", "ATH_STKV", "ATH_STKI"],
            "{keys:?}"
        );
        let header = FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LN Reference"));
        assert_eq!(header.get_i32("ATH_STKI"), Some(7));

        let sat = dir.path().join("reference-saturation.fits");
        let mut maps = vec![vec![0.5f32; 4]];
        crate::stacking::ln::write_reference_saturation(&mut maps, 2, 2, &sat, &cards, f64::INFINITY).unwrap();
        let keys = header_keys(&sat);
        assert_eq!(
            keys.iter().take(4).map(String::as_str).collect::<Vec<_>>(),
            ["IMAGETYP", "ATH_STK", "ATH_STKV", "ATH_STKI"],
            "{keys:?}"
        );
        let header = FitsHeader::from_path(&sat).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LN Reference Saturation"));
    }

    #[test]
    fn written_cards_carry_the_run_id_and_the_scanner_skip_card() {
        let dir = tempfile::tempdir().unwrap();
        let r = LnReference {
            width: 4,
            height: 4,
            planes: vec![vec![0.1f32; 16]],
            frames_used: vec![0, 1, 2],
        };
        let path = dir.path().join("reference.fits");
        let cards = vec![Card::new("ATH_STKI", CardValue::Str("run-42".into())).unwrap()];
        write_reference(&r, &path, &cards).unwrap();

        let header = FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LN Reference"));
        // The scanner-skip card, same convention as a master light.
        assert_eq!(header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(header.get_i32("ATH_STKV"), Some(ATH_STK_VERSION as i32));
        assert_eq!(header.get_str("ATH_STKI").as_deref(), Some("run-42"));
    }

    /// T4: the sidecar round-trips bit for bit (NaN included), carries the
    /// scanner-skip and caller cards, and a reader expecting another width,
    /// height or channel count refuses it naming both shapes; the masks the
    /// run reads are the thresholded planes.
    #[test]
    fn the_saturation_sidecar_round_trips_and_refuses_another_shape() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (12usize, 8usize);
        let maps: Vec<Vec<f32>> = (0..3)
            .map(|c| {
                (0..w * h)
                    .map(|i| {
                        if i % 7 == c {
                            f32::NAN
                        } else {
                            (i as f32 * 0.013 + c as f32 * 0.2).sin().abs() * 1.1
                        }
                    })
                    .collect()
            })
            .collect();
        let path = dir.path().join("reference-saturation.fits");
        let cards = vec![
            Card::new("ATH_STKI", CardValue::Str("run-7".into())).unwrap(),
            Card::new("ATH_STKG", CardValue::Str("g".into())).unwrap(),
        ];
        let mut owned = maps.clone();
        let masks = write_reference_saturation(&mut owned, w, h, &path, &cards, f64::INFINITY)
            .unwrap();
        assert_eq!(masks.len(), 3);

        let back = read_reference_saturation(&path, w, h, 3).unwrap();
        assert_eq!(back.len(), 3);
        for (a, b) in maps.iter().zip(&back) {
            let (a, b): (Vec<u32>, Vec<u32>) = (
                a.iter().map(|v| v.to_bits()).collect(),
                b.iter().map(|v| v.to_bits()).collect(),
            );
            assert_eq!(a, b);
        }
        let header = FitsHeader::from_path(&path).unwrap();
        assert_eq!(header.get_str("ATH_STK").as_deref(), Some("T"));
        assert_eq!(header.get_str("ATH_STKI").as_deref(), Some("run-7"));
        assert_eq!(header.get_str("ATH_STKG").as_deref(), Some("g"));

        for (ew, eh, ec) in [(w + 1, h, 3), (w, h - 1, 3), (w, h, 1)] {
            let e = read_reference_saturation(&path, ew, eh, ec).unwrap_err();
            let msg = format!("{e:#}");
            assert!(
                msg.contains(&format!("{w}x{h}x3")) && msg.contains(&format!("{ew}x{eh}x{ec}")),
                "{msg}"
            );
            assert!(read_reference_saturation_masks(&path, ew, eh, ec, 0.75).is_err());
        }

        let masks = read_reference_saturation_masks(&path, w, h, 3, 0.75).unwrap();
        assert_eq!(masks.len(), 3);
        for (m, map) in masks.iter().zip(&maps) {
            assert_eq!(m, &reference_saturation_mask(map, w, h, 0.75));
        }
        assert!(read_reference_saturation(&dir.path().join("missing.fits"), w, h, 3).is_err());
    }
}
