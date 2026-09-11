//! What the banded engine needs from a set of frames: geometry, per-frame
//! sample kinds, a byte budget model and "fill this band". `BandSource`
//! reads files by position; `RegisteredSource` resamples calibrated frames
//! through their registration transforms on the fly (spec §6.1).

use std::sync::atomic::AtomicBool;

use super::banded::{BandPlanes, BandSource, PlaneKind};
use super::IntegrationError;

/// Receives one band's per-frame rejection bits from `integrate_stack` (spec
/// §6.2 "per-frame rejection bitmaps"). `bits` is laid out
/// `[row_in_band][frame][word]` — `rows × n × words_per_row` u64 words, bit
/// `x % 64` of word `x / 64` set when frame `frame` was PRESENT at
/// `(x, y0 + row)` and NOT a survivor (algorithm or range rejection; a
/// missing/non-finite sample is not a rejection). Called from the band
/// loop's single-threaded tail, once per band, in band order.
///
/// Lives here, next to [`FrameSource`], rather than in `stacking::rej`
/// (the implementer): `integration/` never depends on `stacking` (gated on
/// the `solver` feature too — see `engine::LocalNormRow`'s own doc for the
/// same reasoning), so the trait the engine calls through must be defined
/// on the `integration` side of that boundary.
pub trait RejectionBitSink: Sync {
    /// `== ceil(width / 64)` — the source image's full width, not this
    /// band's row count.
    fn words_per_row(&self) -> usize;
    /// `== n`, the frame source's own frame count.
    fn frames(&self) -> usize;
    fn record_band(&self, y0: usize, rows: usize, bits: &[u64]) -> Result<(), IntegrationError>;
}

/// Forced rejections the band loop applies BEFORE any per-pixel rejection
/// algorithm runs (M4c Task 3, spec §6.2's large-scale paragraph, ruling
/// R-M4c-4): a set bit means "this frame's sample at this pixel is not a
/// survivor, whatever the algorithm would have decided". One instance is
/// bound to ONE plane by its own producer — exactly like
/// [`RejectionBitSink`], since `integrate_stack` also consumes one plane at
/// a time (`StackParams` is a per-plane struct, so neither trait carries a
/// plane index).
///
/// [`Self::forced_row`], not a bare per-pixel accessor, is what the band
/// loop calls: the loop visits `pixels × frames` samples, so a `&dyn` call
/// per SAMPLE would cost billions of virtual calls per plane on a real
/// stack (26 Mpx × 200 frames). Fetched once per (frame, row) instead, the
/// bit test that remains is a load and a shift. [`Self::forced`] is the
/// per-pixel convenience over it, for callers and tests that want one
/// pixel's answer rather than a row.
///
/// Lives here, next to [`RejectionBitSink`], for the same reason that trait
/// does: `integration/` never depends on `stacking`, so the trait the
/// engine calls through must be defined on the `integration` side of that
/// boundary (the implementation — `stacking::rej::RejForcedSource`, over a
/// group's `.rejl` files — is on the other).
pub trait RejectionBitSource: Sync {
    /// `== ceil(width / 64)` — the source image's full width, checked
    /// against the frame source's own geometry before the band loop starts.
    fn words_per_row(&self) -> usize;
    /// `== n`, the frame source's own frame count.
    fn frames(&self) -> usize;
    /// Frame `frame`'s forced bits for the ABSOLUTE image row `y`:
    /// [`Self::words_per_row`] u64 words, bit `x % 64` of word `x / 64` set
    /// when that pixel is forced. `None` when nothing is forced for that
    /// (frame, row) — the common case by a wide margin, and the one the
    /// band loop answers without touching any bits at all.
    fn forced_row(&self, frame: usize, y: usize) -> Option<&[u64]>;
    /// One pixel's answer, derived from [`Self::forced_row`].
    fn forced(&self, frame: usize, x: usize, y: usize) -> bool {
        self.forced_row(frame, y)
            .is_some_and(|words| (words[x / 64] >> (x % 64)) & 1 != 0)
    }
}

pub trait FrameSource: Sync {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn frame_count(&self) -> usize;
    fn plane_kinds(&self) -> Vec<PlaneKind>;
    /// Source bytes one row of every frame costs (drives the band budget).
    fn bytes_per_row(&self) -> usize;
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize;
    /// Fill rows `[y0, y0 + rows)` of every frame into `out`, calling
    /// `on_bytes` once per frame with that frame's accounted share of the
    /// band (see the contract below — not its true disk traffic), checking
    /// `cancel` between frames.
    /// Contract: the values passed to `on_bytes` over one band sum to
    /// exactly `rows × bytes_per_row()` — the engine derives `bytes_total`
    /// from `bytes_per_row`, so a source that reports its true disk traffic
    /// in different units makes the progress overshoot and regress.
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
