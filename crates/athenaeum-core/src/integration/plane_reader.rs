//! Positional reads of a row window of ONE plane of ONE uncompressed FITS
//! file — the reader behind lazy registration, measurement and drizzle,
//! which all read calibrated frames (1 plane mono, 3 planes debayered OSC)
//! one frame at a time. Decode shares `PlaneKind` with the banded reader.

use std::fs::File;
use std::path::Path;

use super::banded::{plane_kind_for_bitpix, pread_exact, probe_fits, PlaneKind};
use super::IntegrationError;

pub struct PlaneReader {
    file: File,
    width: usize,
    height: usize,
    channels: usize,
    kind: PlaneKind,
    data_offset: u64,
}

impl PlaneReader {
    pub fn open(path: &Path) -> Result<PlaneReader, IntegrationError> {
        let (file, info) = probe_fits(path).ok_or_else(|| {
            IntegrationError::BadInput(format!(
                "{}: not an uncompressed 1- or 3-plane FITS image",
                path.display()
            ))
        })?;
        Ok(PlaneReader {
            file,
            width: info.w,
            height: info.h,
            channels: info.naxis3,
            kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
            data_offset: info.data_offset,
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn kind(&self) -> PlaneKind {
        self.kind
    }

    /// Bytes one plane occupies on disk.
    fn plane_bytes(&self) -> u64 {
        (self.width * self.height * self.kind.bytes_per_sample()) as u64
    }

    /// Rows `[y0, y0 + rows)` of `plane`, decoded into `dst` (len `rows × width`).
    pub fn read_rows(
        &self,
        plane: usize,
        y0: usize,
        rows: usize,
        dst: &mut [f32],
    ) -> Result<(), IntegrationError> {
        if plane >= self.channels {
            return Err(IntegrationError::BadInput(format!(
                "plane {plane} of a {}-plane image",
                self.channels
            )));
        }
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!(
                "rows {y0}+{rows} beyond height {}",
                self.height
            )));
        }
        if dst.len() != rows * self.width {
            return Err(IntegrationError::BadInput(format!(
                "destination holds {} samples, {} rows need {}",
                dst.len(),
                rows,
                rows * self.width
            )));
        }
        if rows == 0 {
            return Ok(());
        }
        let bpp = self.kind.bytes_per_sample();
        let mut raw = vec![0u8; rows * self.width * bpp];
        let offset =
            self.data_offset + plane as u64 * self.plane_bytes() + (y0 * self.width * bpp) as u64;
        pread_exact(&self.file, &mut raw, offset)?;
        self.kind.decode_run(&raw, 0, dst);
        Ok(())
    }

    pub fn read_plane(&self, plane: usize) -> Result<Vec<f32>, IntegrationError> {
        let mut out = vec![0f32; self.width * self.height];
        self.read_rows(plane, 0, self.height, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::banded::BandSource;
    use std::io::Write;

    fn f32_fixture(
        dir: &std::path::Path,
        name: &str,
        w: usize,
        h: usize,
        channels: usize,
    ) -> (std::path::PathBuf, Vec<f32>) {
        let data: Vec<f32> = (0..w * h * channels)
            .map(|i| i as f32 * 0.5 - 7.0)
            .collect();
        let p = dir.join(name);
        write_fits_f32(&p, w, h, channels, &data, &[]).unwrap();
        (p, data)
    }

    /// Minimal BITPIX=16 writer (unsigned convention BZERO=32768).
    fn u16_fixture(
        dir: &std::path::Path,
        name: &str,
        w: usize,
        h: usize,
    ) -> (std::path::PathBuf, Vec<u16>) {
        let p = dir.join(name);
        let mut header = Vec::new();
        for line in [
            format!("{:<80}", "SIMPLE  =                    T"),
            format!("{:<80}", "BITPIX  =                   16"),
            format!("{:<80}", "NAXIS   =                    2"),
            format!("{:<80}", format!("NAXIS1  = {:>20}", w)),
            format!("{:<80}", format!("NAXIS2  = {:>20}", h)),
            format!("{:<80}", "BZERO   =              32768.0"),
            format!("{:<80}", "BSCALE  =                  1.0"),
            format!("{:<80}", "END"),
        ] {
            header.extend_from_slice(line.as_bytes());
        }
        header.resize(2880, b' ');
        let vals: Vec<u16> = (0..w * h).map(|i| (1000 + i * 3) as u16).collect();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        let mut data = Vec::with_capacity(w * h * 2);
        for &v in &vals {
            let raw = (v as i32 - 32768) as i16;
            data.extend_from_slice(&raw.to_be_bytes());
        }
        let pad = (2880 - data.len() % 2880) % 2880;
        data.extend(std::iter::repeat(0u8).take(pad));
        f.write_all(&data).unwrap();
        (p, vals)
    }

    #[test]
    fn reads_rows_of_a_single_plane_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "mono.fits", 9, 7, 1);
        let r = PlaneReader::open(&p).unwrap();
        assert_eq!((r.width(), r.height(), r.channels()), (9, 7, 1));
        let mut dst = vec![0f32; 3 * 9];
        r.read_rows(0, 2, 3, &mut dst).unwrap();
        assert_eq!(&dst[..], &data[2 * 9..5 * 9]);
        assert_eq!(r.read_plane(0).unwrap(), data);
    }

    #[test]
    fn reads_the_third_plane_of_an_rgb_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, data) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        let r = PlaneReader::open(&p).unwrap();
        assert_eq!(r.channels(), 3);
        let mut dst = vec![0f32; 2 * 6];
        r.read_rows(2, 3, 2, &mut dst).unwrap();
        let plane = 2 * 6 * 5;
        assert_eq!(&dst[..], &data[plane + 3 * 6..plane + 5 * 6]);
    }

    #[test]
    fn decodes_bzero_scaled_sixteen_bit_data() {
        let dir = tempfile::tempdir().unwrap();
        let (p, vals) = u16_fixture(dir.path(), "u16.fits", 5, 4);
        let r = PlaneReader::open(&p).unwrap();
        let got = r.read_plane(0).unwrap();
        for (g, v) in got.iter().zip(vals.iter()) {
            assert_eq!(*g, *v as f32);
        }
    }

    #[test]
    fn out_of_range_requests_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "mono.fits", 4, 4, 1);
        let r = PlaneReader::open(&p).unwrap();
        let mut dst = vec![0f32; 4];
        assert!(matches!(
            r.read_rows(1, 0, 1, &mut dst),
            Err(IntegrationError::BadInput(_))
        ));
        assert!(matches!(
            r.read_rows(0, 3, 2, &mut dst),
            Err(IntegrationError::BadInput(_))
        ));
        let mut short = vec![0f32; 3];
        assert!(matches!(
            r.read_rows(0, 0, 1, &mut short),
            Err(IntegrationError::BadInput(_))
        ));
    }

    #[test]
    fn a_three_plane_file_still_takes_the_band_source_spill_error() {
        // `BandSource` must keep refusing multi-plane frames exactly as
        // before this task — through the spill path's 1-channel message.
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        let err = BandSource::open(&[p], dir.path(), 1)
            .err()
            .expect("must be rejected");
        assert!(format!("{err}").contains("1-channel"), "{err}");
    }

    #[test]
    fn probe_bitpix_still_answers_none_for_a_three_plane_file() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _) = f32_fixture(dir.path(), "rgb.fits", 6, 5, 3);
        assert_eq!(crate::integration::banded::probe_bitpix(&p), None);
        let (m, _) = f32_fixture(dir.path(), "mono.fits", 6, 5, 1);
        assert_eq!(crate::integration::banded::probe_bitpix(&m), Some(-32));
    }
}
