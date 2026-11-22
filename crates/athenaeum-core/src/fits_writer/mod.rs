//! Standards-compliant FITS writer (FITS 4.0): BITPIX=-32 primary HDU, typed keyword vocabulary.
//!
//! The writer core (cards, the FITS and XISF writers, `Durability`, `XisfBounds`, `OutputFormat`) lives in
//! `starloom`; this module re-exports it under the old paths. The XISF writers named here write Athenaeum's own
//! `CreatorApplication` ([`athenaeum_creator`]), where `starloom`'s plain names write `starloom <version>`.
pub use starloom::fits_writer::{card, writer};
pub mod header_patch;
pub mod keywords;
pub mod xisf_patch;
/// The XISF writers under their old paths: the library's items, with the three plain writers that reach the
/// `CreatorApplication` property replaced by wrappers that pass [`athenaeum_creator`].
pub mod xisf_writer {
    pub use starloom::fits_writer::xisf_writer::{escape_xml, xisf_keyword_value};
    use starloom::fits_writer::xisf_writer as lib;
    use starloom::fits_writer::{Card, FitsWriteError};
    use super::{athenaeum_creator, Durability, XisfBounds};
    use std::io::Write;
    use std::path::Path;

    /// The stacking convention: unit-scaled samples (`ATH_CSCL`), `bounds="0:1"`.
    /// Always [`Durability::Durable`] — see [`write_xisf_f32_with`] for a caller that can afford to skip the fsync.
    pub fn write_xisf_f32(
        path: &Path,
        width: usize,
        height: usize,
        channels: usize,
        data: &[f32],
        cards: &[Card],
    ) -> Result<(), FitsWriteError> {
        lib::write_xisf_f32_full(
            path,
            width,
            height,
            channels,
            data,
            cards,
            XisfBounds::Unit,
            Durability::Durable,
            &athenaeum_creator(),
        )
    }

    /// [`lib::write_xisf_f32_with`] with Athenaeum's `CreatorApplication`.
    pub fn write_xisf_f32_with(
        path: &Path,
        width: usize,
        height: usize,
        channels: usize,
        data: &[f32],
        cards: &[Card],
        bounds: XisfBounds,
        durability: Durability,
    ) -> Result<(), FitsWriteError> {
        lib::write_xisf_f32_full(
            path,
            width,
            height,
            channels,
            data,
            cards,
            bounds,
            durability,
            &athenaeum_creator(),
        )
    }

    /// [`write_xisf_f32_with`]'s streaming body — the whole unit, header and attachment, into one sink.
    pub fn write_xisf_f32_to<W: Write>(
        w: W,
        width: usize,
        height: usize,
        channels: usize,
        data: &[f32],
        cards: &[Card],
        bounds: XisfBounds,
    ) -> Result<(), FitsWriteError> {
        lib::write_xisf_f32_to_full(w, width, height, channels, data, cards, bounds, &athenaeum_creator())
    }
}
// Consumes plate_solve::storage::PlateSolveRecord, which is itself gated —
// plate_solve builds on astroimage (render) and solvemyastro (solver).
#[cfg(all(feature = "render", feature = "solver"))]
pub mod wcs;
pub use card::{Card, CardValue, FitsWriteError};
pub use starloom::fits_writer::{Durability, OutputFormat, XisfBounds};
pub use writer::{
    write_fits_f32, write_fits_f32_streaming_to, write_fits_f32_streaming_with, write_fits_f32_to,
    write_fits_f32_with,
};
pub use xisf_writer::{write_xisf_f32, write_xisf_f32_to, xisf_keyword_value};

/// The `XISF:CreatorApplication` of every file Athenaeum writes: `Athenaeum <athenaeum-core version>`. Evaluated
/// here, in core, so the string is the app's own and not the library's.
pub fn athenaeum_creator() -> String {
    format!("Athenaeum {}", env!("CARGO_PKG_VERSION"))
}

/// One image in the chosen container — the ONE place the two writers are
/// chosen between. `bounds` only matters for XISF. Always
/// [`Durability::Durable`] — see [`write_image_f32_with`] for a caller
/// that can afford to skip the fsync.
pub fn write_image_f32(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
) -> Result<(), card::FitsWriteError> {
    starloom::fits_writer::write_image_f32_full(
        path,
        width,
        height,
        channels,
        data,
        cards,
        format,
        bounds,
        Durability::Durable,
        &athenaeum_creator(),
    )
}

/// [`write_image_f32`] with the durability as a parameter (perf tier 1 Task
/// 6) — the stacking run's regenerable calibrated intermediates pass
/// [`Durability::Volatile`]; every other caller (masters, exports, sends,
/// Perseus) passes [`Durability::Durable`].
pub fn write_image_f32_with(
    path: &std::path::Path,
    width: usize,
    height: usize,
    channels: usize,
    data: &[f32],
    cards: &[card::Card],
    format: OutputFormat,
    bounds: XisfBounds,
    durability: Durability,
) -> Result<(), card::FitsWriteError> {
    starloom::fits_writer::write_image_f32_full(
        path,
        width,
        height,
        channels,
        data,
        cards,
        format,
        bounds,
        durability,
        &athenaeum_creator(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard for the move of the writer core to `starloom`: a file written through Athenaeum's own writer names
    /// Athenaeum, and its version, in the XISF `CreatorApplication` property, never the library's own name.
    #[test]
    fn athenaeum_xisf_creator_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creator.xisf");
        let data: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        write_xisf_f32(&path, 4, 4, 1, &data, &[]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let marker = "<Property id=\"XISF:CreatorApplication\" type=\"String\" value=\"";
        let start = text.find(marker).expect("the CreatorApplication property is written") + marker.len();
        let creator = &text[start..start + text[start..].find('"').expect("the property value is closed")];
        assert_eq!(creator, format!("Athenaeum {}", env!("CARGO_PKG_VERSION")));
        assert!(!creator.contains("starloom"), "{creator}");
        // The same through the container switch.
        let path2 = dir.path().join("creator2.xisf");
        write_image_f32(&path2, 4, 4, 1, &data, &[], OutputFormat::Xisf, XisfBounds::Unit).unwrap();
        let text2 = String::from_utf8_lossy(&std::fs::read(&path2).unwrap()).into_owned();
        assert!(text2.contains(&format!("{marker}Athenaeum {}\"", env!("CARGO_PKG_VERSION"))), "{text2}");
    }

    /// Every other XISF writer reachable from core names Athenaeum too: the durability and sink variants, and the
    /// container switch with its durability parameter.
    #[test]
    fn athenaeum_xisf_creator_reaches_every_writer() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        let want = format!("<Property id=\"XISF:CreatorApplication\" type=\"String\" value=\"Athenaeum {}\"", env!("CARGO_PKG_VERSION"));

        let p = dir.path().join("with.xisf");
        xisf_writer::write_xisf_f32_with(&p, 4, 4, 1, &data, &[], XisfBounds::Unit, Durability::Volatile).unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
        assert!(text.contains(&want), "write_xisf_f32_with: {text}");

        let mut buf: Vec<u8> = Vec::new();
        xisf_writer::write_xisf_f32_to(&mut buf, 4, 4, 1, &data, &[], XisfBounds::Unit).unwrap();
        let text = String::from_utf8_lossy(&buf).into_owned();
        assert!(text.contains(&want), "write_xisf_f32_to: {text}");

        let p = dir.path().join("image_with.xisf");
        write_image_f32_with(&p, 4, 4, 1, &data, &[], OutputFormat::Xisf, XisfBounds::Unit, Durability::Volatile)
            .unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
        assert!(text.contains(&want), "write_image_f32_with: {text}");
    }

    /// Core's container switch passes its caller's `bounds` through (starloom phase 1): a file written with
    /// `XisfBounds::Adu16` carries `bounds="0:65535"` and equals the direct call to starloom's full form, both with
    /// Athenaeum's creator, once the wall-clock `XISF:CreationTime` line is dropped.
    #[test]
    fn core_write_image_f32_keeps_the_callers_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        let via_core = dir.path().join("core.xisf");
        write_image_f32(&via_core, 4, 4, 1, &data, &[], OutputFormat::Xisf, XisfBounds::Adu16).unwrap();
        let direct = dir.path().join("direct.xisf");
        starloom::fits_writer::write_image_f32_full(
            &direct,
            4,
            4,
            1,
            &data,
            &[],
            OutputFormat::Xisf,
            XisfBounds::Adu16,
            Durability::Durable,
            &athenaeum_creator(),
        )
        .unwrap();
        let without_clock = |p: &std::path::Path| -> String {
            String::from_utf8_lossy(&std::fs::read(p).unwrap())
                .lines()
                .filter(|l| !l.contains("XISF:CreationTime"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (a, b) = (without_clock(&via_core), without_clock(&direct));
        assert!(a.contains("bounds=\"0:65535\""), "{a}");
        assert!(b.contains("bounds=\"0:65535\""), "{b}");
        assert_eq!(a, b);
    }
}
