//! Local normalization (M2, spec §5.2): per-frame `A(x,y)`/`B(x,y)` grids in
//! the reference geometry, applied as `v' = A·v + B`. `grid` holds the grid
//! type itself (`LnGrid`), the bicubic B-spline evaluator that turns the
//! coarse stride grid into per-pixel `A`/`B` values, and the `.athln` binary
//! sidecar one frame's grids (one per channel, `LnFrameGrids`) round-trip
//! through. Later M2 tasks (measurement, application, provenance) build on
//! this foundation.

pub mod grid;

pub use grid::{LnFrameGrids, LnGrid};
