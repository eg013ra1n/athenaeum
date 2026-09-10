//! Local normalization (M2, spec §5.2): per-frame `A(x,y)`/`B(x,y)` grids in
//! the reference geometry, applied as `v' = A·v + B`. `grid` holds the grid
//! type itself (`LnGrid`), the bicubic B-spline evaluator that turns the
//! coarse stride grid into per-pixel `A`/`B` values, and the `.athln` binary
//! sidecar one frame's grids (one per channel, `LnFrameGrids`) round-trip
//! through. `background` (M2 Task 2) is the per-plane robust background
//! model on the same stride mesh — the input Task 5 builds `B = B_ref −
//! s·B_tgt` from. Later M2 tasks (measurement, application, provenance)
//! build on this foundation.

pub mod background;
pub mod grid;

pub use background::{background_grid, BackgroundGrid, BackgroundParams, DEFAULT_PARAMS};
pub use grid::{LnFrameGrids, LnGrid};
