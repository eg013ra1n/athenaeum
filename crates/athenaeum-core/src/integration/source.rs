//! What the banded engine needs from a set of frames: geometry, per-frame
//! sample kinds, a byte budget model and "fill this band". `BandSource`
//! reads files by position; `RegisteredSource` resamples calibrated frames
//! through their registration transforms on the fly (spec §6.1).

use std::sync::atomic::AtomicBool;

use super::banded::{BandPlanes, BandSource, PlaneKind};
use super::IntegrationError;

pub trait FrameSource: Sync {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn frame_count(&self) -> usize;
    fn plane_kinds(&self) -> Vec<PlaneKind>;
    /// Source bytes one row of every frame costs (drives the band budget).
    fn bytes_per_row(&self) -> usize;
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize;
    /// Fill rows `[y0, y0 + rows)` of every frame into `out`, calling
    /// `on_bytes` once per frame with the bytes read from disk, checking
    /// `cancel` between frames.
    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError>;
}

impl FrameSource for BandSource {
    fn width(&self) -> usize {
        BandSource::width(self)
    }
    fn height(&self) -> usize {
        BandSource::height(self)
    }
    fn frame_count(&self) -> usize {
        BandSource::frame_count(self)
    }
    fn plane_kinds(&self) -> Vec<PlaneKind> {
        BandSource::plane_kinds(self)
    }
    fn bytes_per_row(&self) -> usize {
        BandSource::bytes_per_row(self)
    }
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
        BandSource::band_rows_for_budget(self, budget_bytes)
    }
    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError> {
        BandSource::read_band_with_progress(self, y0, rows, out, concurrency, on_bytes, cancel)
    }
}
