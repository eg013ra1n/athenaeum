//! The complete subject ↔ reference pixel mapping a registered frame
//! carries: a linear model, its cached inverse, and an optional distortion
//! layer — a polynomial ([`super::polynomial`]) or, since M4c, a
//! thin-plate spline ([`super::tps`]). Serialized as the `transform_json`
//! of `registration_results` (spec §9.1).

use std::sync::{Arc, RwLock};

use rayon::prelude::*;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use super::linear::Linear;
use super::polynomial::Distortion;
use super::tps::{ThinPlateSpline, TPS_GRID_PX};

/// Reference → subject mapping, the only direction a gather-style resampler
/// needs.
pub trait InverseMap: Sync {
    fn inverse(&self, x: f64, y: f64) -> (f64, f64);

    /// An evaluator for a BURST of queries — one band, one row, one
    /// window probe (ruling R-T4-6b).
    ///
    /// The default is `self.inverse` per call, which is what every map
    /// without a cache wants. [`PixelMap`] overrides it to capture its
    /// displacement-grid handle ONCE, so a million-pixel warp neither
    /// takes the cache lock per pixel nor can have its grid released out
    /// from under it mid-band. The `Box` costs one allocation per burst
    /// and the call costs exactly what `&dyn InverseMap` already cost.
    fn inverse_burst(&self) -> Box<dyn Fn(f64, f64) -> (f64, f64) + Send + Sync + '_> {
        Box::new(move |x, y| self.inverse(x, y))
    }
}

impl InverseMap for Linear {
    /// A bare `Linear` used as an inverse map is applied as-is: callers hand
    /// in the already-inverted matrix (or the identity).
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        self.apply(x, y)
    }
}

/// A bilinear displacement grid over ONE direction of a
/// [`DistortionModel::Tps`] map's domain, sampled every [`TPS_GRID_PX`]
/// pixels (ruling R-M4c-5).
///
/// Evaluating the spline itself costs one `ln` per node per pixel — at
/// the 600-node cap that is 600 logarithms for every one of a 26 Mpx
/// frame's pixels. The grid pays that once per sample point instead (a
/// 1024th of the pixels) and reads a bilinear tap between samples, which
/// on a field as smooth as a registration residual is exact to a small
/// fraction of a milli-pixel.
///
/// A grid is ONLY for pixel work. Anything that evaluates the map a few
/// hundred times — residual statistics, star re-pairing, a corner
/// displacement — must go through the exact spline instead
/// ([`PixelMap::forward_exact`] / [`PixelMap::inverse_exact`]); building
/// a 540 k-sample grid to answer 600 queries costs three orders of
/// magnitude more than answering them directly (fix round 1, ruling
/// R-T4-3).
///
/// Never serialized: `transform_json` stores the splines, and the grid is
/// rebuilt lazily on first use.
///
/// NOT `Clone`, deliberately: the whole point of the cache is that one
/// grid is shared through an `Arc`, and a `Clone` would let a caller
/// silently reintroduce the per-map copy ruling R-T4-3b removed (and
/// would break the alive-grid accounting below).
#[derive(Debug)]
pub struct TpsGrid {
    /// Sample origin (the domain's lower corner) in reference pixels.
    x0: f64,
    y0: f64,
    /// Domain's upper corner: a query is clamped into `[x0, x1] × [y0, y1]`
    /// before it is sampled, so a pixel outside the fitted region gets the
    /// nearest edge's displacement instead of a spline extrapolation —
    /// the same guard [`Distortion`]'s own `domain` applies.
    x1: f64,
    y1: f64,
    step: f64,
    nx: usize,
    ny: usize,
    samples: Vec<(f32, f32)>,
}

impl TpsGrid {
    /// Samples `spline` over `domain` at [`TPS_GRID_PX`] spacing, plus one
    /// margin cell each way so the last cell is complete. Rows are
    /// independent, so the build runs over `rayon`; it is a pure function
    /// of its inputs either way.
    pub fn build(spline: &ThinPlateSpline, domain: [f64; 4]) -> TpsGrid {
        let step = TPS_GRID_PX as f64;
        let [x0, y0, x1, y1] = domain;
        let nx = ((x1 - x0) / step).ceil().max(0.0) as usize + 2;
        let ny = ((y1 - y0) / step).ceil().max(0.0) as usize + 2;
        let samples: Vec<(f32, f32)> = (0..ny)
            .into_par_iter()
            .flat_map_iter(|j| {
                let y = y0 + j as f64 * step;
                (0..nx).map(move |col| {
                    let x = x0 + col as f64 * step;
                    let (dx, dy) = spline.displacement(x, y);
                    (dx as f32, dy as f32)
                })
            })
            .collect();
        TpsGrid {
            x0,
            y0,
            x1,
            y1,
            step,
            nx,
            ny,
            samples,
        }
    }

    /// The displacement at `(x, y)`, bilinear between samples, the query
    /// clamped into the fitted domain first.
    #[inline]
    pub fn displacement_at(&self, x: f64, y: f64) -> (f64, f64) {
        let fx = (x.clamp(self.x0, self.x1) - self.x0) / self.step;
        let fy = (y.clamp(self.y0, self.y1) - self.y0) / self.step;
        let i = (fx.floor().max(0.0) as usize).min(self.nx - 2);
        let j = (fy.floor().max(0.0) as usize).min(self.ny - 2);
        let tx = fx - i as f64;
        let ty = fy - j as f64;
        let row = j * self.nx + i;
        let g = &self.samples;
        let (a, b) = (g[row], g[row + 1]);
        let (c, d) = (g[row + self.nx], g[row + self.nx + 1]);
        let lerp = |p: f64, q: f64, t: f64| p + (q - p) * t;
        let dx_top = lerp(a.0 as f64, b.0 as f64, tx);
        let dx_bot = lerp(c.0 as f64, d.0 as f64, tx);
        let dy_top = lerp(a.1 as f64, b.1 as f64, tx);
        let dy_bot = lerp(c.1 as f64, d.1 as f64, tx);
        (lerp(dx_top, dx_bot, ty), lerp(dy_top, dy_bot, ty))
    }

    /// Bytes this grid's samples occupy — what
    /// [`PixelMap::release_grids`] gives back, and what a stage's memory
    /// budget is counted in.
    pub fn bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<(f32, f32)>()
    }
}

/// How many displacement grids exist RIGHT NOW, and the most that ever
/// existed at once (test builds only).
///
/// This is the instrument ruling R-T4-6d's run-level pin reads: a `tps`
/// run's memory problem is not how many grids it builds over its life
/// (rebuilding is cheap and expected) but how many are resident at the
/// same moment — 208 of them, at ~8.6 MB each, is what thrashed the
/// acceptance machine. `ALIVE` is decremented by [`TpsGrid`]'s own
/// `Drop`, so it counts grids however they go away: an explicit
/// [`PixelMap::release_grids`], or the last handle simply falling out of
/// scope.
#[cfg(test)]
pub(crate) mod grid_counters {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    pub static BUILDS: AtomicUsize = AtomicUsize::new(0);
    pub static ALIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK_ALIVE: AtomicUsize = AtomicUsize::new(0);

    /// Held by EVERY test that builds a displacement grid.
    ///
    /// The counters are process-global and the harness runs tests in
    /// parallel, so without this a grid another test happened to hold
    /// would inflate the measuring test's `PEAK_ALIVE` — its bound is
    /// deliberately tight (one grid per worker, not one per frame), so
    /// even three stray grids could flip it. The lock is cheap: every
    /// test that takes it is short, and the one long holder (the 20-frame
    /// run pin) is the only thing that needs the quiet.
    ///
    /// **A new test that builds a `tps` grid must take this too**, or it
    /// will make the run pin flaky rather than itself.
    static EXCLUSIVE: Mutex<()> = Mutex::new(());

    pub fn exclusive() -> MutexGuard<'static, ()> {
        EXCLUSIVE.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn on_build() {
        BUILDS.fetch_add(1, Ordering::Relaxed);
        let now = ALIVE.fetch_add(1, Ordering::Relaxed) + 1;
        PEAK_ALIVE.fetch_max(now, Ordering::Relaxed);
    }

    pub(super) fn on_drop() {
        ALIVE.fetch_sub(1, Ordering::Relaxed);
    }

    /// Zeroes every counter — a test that measures a run calls this first.
    /// Tests that share a process must not measure concurrently; the run
    /// pins that use this are `#[serial]`-shaped by construction (one run
    /// thread at a time).
    pub fn reset() {
        BUILDS.store(0, Ordering::Relaxed);
        ALIVE.store(0, Ordering::Relaxed);
        PEAK_ALIVE.store(0, Ordering::Relaxed);
    }

    pub fn snapshot() -> (usize, usize, usize) {
        (
            BUILDS.load(Ordering::Relaxed),
            ALIVE.load(Ordering::Relaxed),
            PEAK_ALIVE.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
impl Drop for TpsGrid {
    fn drop(&mut self) {
        grid_counters::on_drop();
    }
}

/// One direction's cache slot: empty, or holding a built grid every
/// clone of the map shares.
///
/// `RwLock<Option<Arc<…>>>` rather than a `OnceLock` because a built grid
/// must be RELEASABLE (ruling R-T4-6) and a `OnceLock` cannot be reset.
/// The `Arc` inside is what keeps the per-pixel path off the lock: a
/// caller takes ONE handle for a row or a band
/// ([`PixelMap::inverse_burst`], [`PixelMap::forward_eval`]) and samples
/// through it, so a release during that burst frees the slot while the
/// burst's own handle keeps its grid alive to the end.
type GridSlot = RwLock<Option<Arc<TpsGrid>>>;

/// The two lazily-built displacement grids of one spline pair, shared by
/// every clone of the [`PixelMap`] that owns them (fix round 1, ruling
/// R-T4-3b/c) and releasable through any of them (fix round 2, ruling
/// R-T4-6).
///
/// `Clone` on a `OnceLock<TpsGrid>` COPIES the built grid, and a
/// registered frame's map is cloned into run state five times over
/// (`rc.measured`, `GroupMember`, `StackFrame`, `RegisteredFrame`,
/// `RegisteredSource`) — at 8.6 MB a direction that was gigabytes of
/// duplicate. Behind an `Arc` every clone shares one allocation.
///
/// The two directions are independent cells because they have
/// independent consumers: the resampler only ever asks for `inverse`,
/// drizzle only for `forward`, and neither should pay for the other.
#[derive(Debug, Default)]
pub struct TpsGrids {
    forward: GridSlot,
    inverse: GridSlot,
}

impl TpsGrids {
    /// The slot's grid, built through `build` on first use. Read-locked
    /// on the hot path (every subsequent burst), write-locked only to
    /// install a freshly built grid — and the double check inside the
    /// write lock means two threads racing a rebuild install one grid,
    /// not two.
    fn get_or_build(slot: &GridSlot, build: impl FnOnce() -> TpsGrid) -> Arc<TpsGrid> {
        if let Some(g) = slot.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return Arc::clone(g);
        }
        let mut w = slot.write().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = w.as_ref() {
            return Arc::clone(g);
        }
        let g = Arc::new(build());
        #[cfg(test)]
        grid_counters::on_build();
        *w = Some(Arc::clone(&g));
        g
    }
}

/// Which way an [`EvalLayer`] is being asked to displace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Direction {
    Forward,
    Inverse,
}

/// One direction's distortion, resolved once for a burst of pixel
/// queries (ruling R-T4-6b).
///
/// The spline arm holds an `Arc<TpsGrid>`: taken once when the evaluator
/// is built, so the per-pixel path is a bounds-checked slice read and
/// nothing else — and so a [`PixelMap::release_grids`] on another thread
/// cannot free the grid this burst is reading.
enum EvalLayer<'a> {
    None,
    Polynomial(&'a Distortion, Direction),
    Grid(Arc<TpsGrid>),
}

impl<'a> EvalLayer<'a> {
    fn of(distortion: Option<&'a DistortionModel>, dir: Direction) -> EvalLayer<'a> {
        match distortion {
            None => EvalLayer::None,
            Some(DistortionModel::Polynomial(d)) => EvalLayer::Polynomial(d, dir),
            Some(model @ DistortionModel::Tps { .. }) => {
                let grid = match dir {
                    Direction::Forward => model.forward_grid(),
                    Direction::Inverse => model.inverse_grid(),
                };
                match grid {
                    Some(g) => EvalLayer::Grid(g),
                    // Unreachable: the spline arm always returns a grid.
                    None => EvalLayer::None,
                }
            }
        }
    }

    #[inline]
    fn displacement(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            EvalLayer::None => (0.0, 0.0),
            EvalLayer::Polynomial(d, Direction::Forward) => d.forward_displacement(x, y),
            EvalLayer::Polynomial(d, Direction::Inverse) => d.inverse_displacement(x, y),
            EvalLayer::Grid(g) => g.displacement_at(x, y),
        }
    }
}

/// Releases a map's displacement grids when it goes out of scope —
/// [`PixelMap::release_grids_on_drop`].
///
/// For a caller whose per-frame pixel work has fallible steps in it: an
/// early `?` between the first warp and the explicit release would
/// otherwise leak that frame's grid for the rest of the run, and a
/// per-frame failure is routinely non-fatal upstream (a frame that fails
/// to register is excluded, not fatal to the run).
pub struct GridRelease<'a>(&'a PixelMap);

impl Drop for GridRelease<'_> {
    fn drop(&mut self) {
        self.0.release_grids();
    }
}

/// Subject → reference, with the displacement grid captured — see
/// [`PixelMap::forward_eval`].
pub struct ForwardEval<'a> {
    linear: &'a Linear,
    layer: EvalLayer<'a>,
}

impl ForwardEval<'_> {
    /// Byte-identical to [`PixelMap::forward`] for every arm.
    #[inline]
    pub fn at(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.linear.apply(x, y);
        match &self.layer {
            EvalLayer::None => (px, py),
            layer => {
                let (dx, dy) = layer.displacement(px, py);
                (px + dx, py + dy)
            }
        }
    }
}

/// Reference → subject, with the displacement grid captured — see
/// [`PixelMap::inverse_eval`].
pub struct InverseEval<'a> {
    linear_inv: &'a Linear,
    layer: EvalLayer<'a>,
}

impl InverseEval<'_> {
    /// Byte-identical to [`PixelMap::inverse`] for every arm.
    #[inline]
    pub fn at(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match &self.layer {
            EvalLayer::None => (x, y),
            layer => {
                let (dx, dy) = layer.displacement(x, y);
                (x + dx, y + dy)
            }
        };
        self.linear_inv.apply(rx, ry)
    }
}

/// The distortion layer sitting on top of a [`PixelMap`]'s linear model
/// (M4c, ruling R-M4c-6): the SIP-style polynomial M1 shipped, or a
/// thin-plate spline.
///
/// Tagged explicitly on the wire (`{"kind": "polynomial", …}` /
/// `{"kind": "tps", …}`). A stored `transform_json` written before M4c
/// carries no tag at all, and every one of those is a polynomial — see
/// the hand-written [`Deserialize`] below, which is the whole reason this
/// enum is not a plain derive.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DistortionModel {
    Polynomial(Distortion),
    Tps {
        forward: ThinPlateSpline,
        inverse: ThinPlateSpline,
        /// Reference-pixel box `[x0, y0, x1, y1]` the splines were fitted
        /// over (the nodes' bounding box, each side inflated by
        /// [`super::polynomial::DOMAIN_MARGIN`]). Evaluation clamps into
        /// it and the grid covers exactly it.
        domain: [f64; 4],
        /// Displacement grids, built per direction on first PIXEL use,
        /// never serialized, never part of equality, and SHARED by every
        /// clone of this model (ruling R-T4-3b/c). Internal: a caller
        /// reaches them through [`DistortionModel::forward_displacement`]
        /// / [`DistortionModel::inverse_displacement`].
        #[serde(skip)]
        grids: Arc<TpsGrids>,
    },
}

/// The cached grids are a pure function of the two splines and the domain,
/// so two models that agree on those three ARE equal — a map that has
/// already been evaluated must compare equal to a freshly deserialized
/// one, and a clone that shares its parent's grids must compare equal to
/// one that has none.
impl PartialEq for DistortionModel {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DistortionModel::Polynomial(a), DistortionModel::Polynomial(b)) => a == b,
            (
                DistortionModel::Tps {
                    forward: f1,
                    inverse: i1,
                    domain: d1,
                    ..
                },
                DistortionModel::Tps {
                    forward: f2,
                    inverse: i2,
                    domain: d2,
                    ..
                },
            ) => f1 == f2 && i1 == i2 && d1 == d2,
            _ => false,
        }
    }
}

/// The `Tps` variant's wire shape, minus the tag and the cache.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TpsWire {
    forward: ThinPlateSpline,
    inverse: ThinPlateSpline,
    domain: [f64; 4],
}

impl<'de> Deserialize<'de> for DistortionModel {
    /// Reads the `kind` tag, defaulting to `"polynomial"` when it is
    /// absent: that is every `transform_json` M1–M4b ever wrote, and a
    /// derived internally-tagged enum would reject all of them.
    ///
    /// **JSON only.** The tag default needs to look at the input before
    /// choosing a shape, which it does by buffering through
    /// `serde_json::Value` — so this decodes from a JSON deserializer and
    /// nothing else. That is the whole world it lives in
    /// (`registration_results.transform_json`, [`PixelMap::from_json`]);
    /// a `PixelMap` has never travelled over postcard or any other
    /// format, and a future one would need a hand-written tag default of
    /// its own.
    fn deserialize<D>(deserializer: D) -> Result<DistortionModel, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let kind = value
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("polynomial")
            .to_string();
        match kind.as_str() {
            "polynomial" => Distortion::deserialize(&value)
                .map(DistortionModel::Polynomial)
                .map_err(D::Error::custom),
            "tps" => {
                let w = TpsWire::deserialize(&value).map_err(D::Error::custom)?;
                Ok(DistortionModel::tps(w.forward, w.inverse, w.domain))
            }
            other => Err(D::Error::custom(format!(
                "unknown distortion kind {other:?}: expected \"polynomial\" or \"tps\""
            ))),
        }
    }
}

impl DistortionModel {
    /// A thin-plate-spline layer with an empty grid cache.
    pub fn tps(
        forward: ThinPlateSpline,
        inverse: ThinPlateSpline,
        domain: [f64; 4],
    ) -> DistortionModel {
        DistortionModel::Tps {
            forward,
            inverse,
            domain,
            grids: Arc::new(TpsGrids::default()),
        }
    }

    /// What [`PixelMap::from_json`] demands of a stored layer before it
    /// trusts it — see each arm's own `is_well_formed`.
    pub fn is_well_formed(&self) -> bool {
        match self {
            DistortionModel::Polynomial(d) => d.is_well_formed(),
            DistortionModel::Tps {
                forward,
                inverse,
                domain,
                ..
            } => {
                forward.is_well_formed()
                    && inverse.is_well_formed()
                    && domain.iter().all(|v| v.is_finite())
                    && domain[0] <= domain[2]
                    && domain[1] <= domain[3]
            }
        }
    }

    /// `"polynomial<order>"` / `"tps"` — the suffix
    /// `registration_results.model` carries.
    pub fn label(&self) -> String {
        match self {
            DistortionModel::Polynomial(d) => format!("polynomial{}", d.order),
            DistortionModel::Tps { .. } => "tps".to_string(),
        }
    }

    /// `(x, y)` clamped into the spline's fitted domain — what the grid
    /// does implicitly, applied to the exact path so the two agree
    /// outside the star-covered region too.
    #[inline]
    fn clamp_to_domain(domain: &[f64; 4], x: f64, y: f64) -> (f64, f64) {
        (
            x.max(domain[0]).min(domain[2]),
            y.max(domain[1]).min(domain[3]),
        )
    }

    /// The forward displacement grid, built on first use. **Take this
    /// ONCE per row or per band** and sample through the handle — calling
    /// it per pixel pays a lock and an atomic refcount per pixel, which is
    /// exactly what ruling R-T4-6(b) forbids. `None` for the polynomial
    /// arm, which has no grid.
    pub fn forward_grid(&self) -> Option<Arc<TpsGrid>> {
        match self {
            DistortionModel::Polynomial(_) => None,
            DistortionModel::Tps {
                forward,
                domain,
                grids,
                ..
            } => Some(TpsGrids::get_or_build(&grids.forward, || {
                TpsGrid::build(forward, *domain)
            })),
        }
    }

    /// The inverse displacement grid, built on first use — see
    /// [`DistortionModel::forward_grid`] for how often to ask.
    pub fn inverse_grid(&self) -> Option<Arc<TpsGrid>> {
        match self {
            DistortionModel::Polynomial(_) => None,
            DistortionModel::Tps {
                inverse,
                domain,
                grids,
                ..
            } => Some(TpsGrids::get_or_build(&grids.inverse, || {
                TpsGrid::build(inverse, *domain)
            })),
        }
    }

    /// Drops both directions' built grids, for every clone of this model
    /// (ruling R-T4-6a): they share one cache, so releasing through any
    /// handle frees it for all of them. A burst that is still running
    /// holds its own `Arc` and finishes on the grid it started with; the
    /// next request after that rebuilds.
    ///
    /// Returns the bytes freed — 0 when nothing was built, and 0 for the
    /// polynomial arm.
    pub fn release_grids(&self) -> usize {
        let DistortionModel::Tps { grids, .. } = self else {
            return 0;
        };
        let mut freed = 0;
        for slot in [&grids.forward, &grids.inverse] {
            if let Some(g) = slot.write().unwrap_or_else(|e| e.into_inner()).take() {
                freed += g.bytes();
            }
        }
        freed
    }

    /// Forward displacement (pixels) at reference-space point `(x, y)` —
    /// the linear model's output. **The PIXEL path**, and the SLOW way to
    /// walk it: every call takes the cache lock. Kept for one-off queries
    /// and for the tests that pin grid-vs-exact agreement; a loop over
    /// pixels wants [`PixelMap::forward_eval`].
    #[inline]
    pub fn forward_displacement(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.forward_displacement(x, y),
            DistortionModel::Tps { .. } => self
                .forward_grid()
                .map(|g| g.displacement_at(x, y))
                .unwrap_or((0.0, 0.0)),
        }
    }

    /// Inverse displacement (pixels) at reference pixel `(x, y)` — see
    /// [`DistortionModel::forward_displacement`] on why a pixel loop wants
    /// a handle instead.
    #[inline]
    pub fn inverse_displacement(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.inverse_displacement(x, y),
            DistortionModel::Tps { .. } => self
                .inverse_grid()
                .map(|g| g.displacement_at(x, y))
                .unwrap_or((0.0, 0.0)),
        }
    }

    /// [`DistortionModel::forward_displacement`] without touching a grid
    /// (ruling R-T4-3a): the exact spline, `O(nodes)`. For the polynomial
    /// arm it is the same call — that arm has no grid.
    ///
    /// This is what everything that is NOT resampling pixels must use:
    /// residual statistics, star re-pairing, a QA measurement, a corner
    /// displacement. Those ask a few hundred questions, and a grid costs
    /// half a million spline evaluations to build.
    #[inline]
    pub fn forward_displacement_exact(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.forward_displacement(x, y),
            DistortionModel::Tps {
                forward, domain, ..
            } => {
                let (x, y) = Self::clamp_to_domain(domain, x, y);
                forward.displacement(x, y)
            }
        }
    }

    /// [`DistortionModel::inverse_displacement`] without touching a grid
    /// (ruling R-T4-3a) — see [`DistortionModel::forward_displacement_exact`].
    #[inline]
    pub fn inverse_displacement_exact(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            DistortionModel::Polynomial(d) => d.inverse_displacement(x, y),
            DistortionModel::Tps {
                inverse, domain, ..
            } => {
                let (x, y) = Self::clamp_to_domain(domain, x, y);
                inverse.displacement(x, y)
            }
        }
    }

    /// Whether each direction's displacement grid has been built yet —
    /// `(forward, inverse)`. Diagnostic: it is what pins that a
    /// non-pixel caller never builds one (ruling R-T4-3a) and that a
    /// clone shares what its parent built (R-T4-3b).
    pub fn grids_built(&self) -> (bool, bool) {
        match self {
            DistortionModel::Polynomial(_) => (false, false),
            DistortionModel::Tps { grids, .. } => {
                let built =
                    |slot: &GridSlot| slot.read().unwrap_or_else(|e| e.into_inner()).is_some();
                (built(&grids.forward), built(&grids.inverse))
            }
        }
    }

    /// The shared grid cache, for the identity check a clone-sharing pin
    /// needs (`Arc::ptr_eq`). `None` for the polynomial arm, which has no
    /// grids at all.
    pub fn grid_cache(&self) -> Option<&Arc<TpsGrids>> {
        match self {
            DistortionModel::Polynomial(_) => None,
            DistortionModel::Tps { grids, .. } => Some(grids),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelMap {
    pub linear: Linear,
    pub linear_inv: Linear,
    pub distortion: Option<DistortionModel>,
}

impl PixelMap {
    pub fn linear(linear: Linear) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap {
            linear,
            linear_inv,
            distortion: None,
        })
    }

    pub fn with_distortion(linear: Linear, distortion: Distortion) -> Option<PixelMap> {
        PixelMap::with_distortion_model(linear, DistortionModel::Polynomial(distortion))
    }

    pub fn with_distortion_model(linear: Linear, distortion: DistortionModel) -> Option<PixelMap> {
        let linear_inv = linear.inverse()?;
        Some(PixelMap {
            linear,
            linear_inv,
            distortion: Some(distortion),
        })
    }

    /// A forward evaluator that holds this map's displacement-grid handle
    /// for its whole lifetime (ruling R-T4-6b).
    ///
    /// **This is what a pixel loop takes** — once per frame, row or band —
    /// and then calls [`ForwardEval::at`] per pixel: no lock, no atomic,
    /// no `match` on the cache. It also pins the grid it captured against
    /// a concurrent [`PixelMap::release_grids`], so a release can never
    /// pull the data out from under a running deposit.
    pub fn forward_eval(&self) -> ForwardEval<'_> {
        ForwardEval {
            linear: &self.linear,
            layer: EvalLayer::of(self.distortion.as_ref(), Direction::Forward),
        }
    }

    /// The reverse direction of [`PixelMap::forward_eval`] — reference
    /// pixel → subject pixel, the gather-style resampler's direction.
    pub fn inverse_eval(&self) -> InverseEval<'_> {
        InverseEval {
            linear_inv: &self.linear_inv,
            layer: EvalLayer::of(self.distortion.as_ref(), Direction::Inverse),
        }
    }

    /// Drops this map's built displacement grids (ruling R-T4-6a),
    /// returning the bytes freed. A no-op for a linear or polynomial map,
    /// which have no grids.
    ///
    /// Every clone of a map shares ONE cache, so this frees the grid for
    /// all of them — which is the point: a registered frame's map lives in
    /// five places at once, and a stage that has finished resampling that
    /// frame should not leave 8.6 MB per direction behind in any of them.
    /// The next pixel request rebuilds lazily (≈ 1 s on a 26 Mpx frame).
    pub fn release_grids(&self) -> usize {
        self.distortion
            .as_ref()
            .map_or(0, DistortionModel::release_grids)
    }

    /// [`PixelMap::release_grids`] bound to a scope, so a fallible caller
    /// releases on EVERY exit path and not just the happy one.
    pub fn release_grids_on_drop(&self) -> GridRelease<'_> {
        GridRelease(self)
    }

    /// Subject pixel → reference pixel. **The PIXEL path** — for a
    /// thin-plate spline this reads (and, on the first call, builds) the
    /// forward displacement grid, taking the cache lock to do it. A loop
    /// over pixels wants [`PixelMap::forward_eval`]; a caller that
    /// evaluates the map a few hundred times wants
    /// [`PixelMap::forward_exact`] (ruling R-T4-3a).
    #[inline]
    pub fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.linear.apply(x, y);
        match &self.distortion {
            None => (px, py),
            Some(d) => {
                let (dx, dy) = d.forward_displacement(px, py);
                (px + dx, py + dy)
            }
        }
    }

    /// Reference pixel → subject pixel. **The PIXEL path** — see
    /// [`PixelMap::forward`]; the gather-style resampler and LN's warp
    /// are the callers this exists for.
    #[inline]
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match &self.distortion {
            None => (x, y),
            Some(d) => {
                let (dx, dy) = d.inverse_displacement(x, y);
                (x + dx, y + dy)
            }
        };
        self.linear_inv.apply(rx, ry)
    }

    /// [`PixelMap::forward`] evaluated through the exact distortion, never
    /// a grid (ruling R-T4-3a). Identical arithmetic for a linear map and
    /// for the polynomial arm; for a spline it is `O(nodes)` instead of a
    /// grid build.
    #[inline]
    pub fn forward_exact(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.linear.apply(x, y);
        match &self.distortion {
            None => (px, py),
            Some(d) => {
                let (dx, dy) = d.forward_displacement_exact(px, py);
                (px + dx, py + dy)
            }
        }
    }

    /// [`PixelMap::inverse`] evaluated through the exact distortion, never
    /// a grid (ruling R-T4-3a).
    #[inline]
    pub fn inverse_exact(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = match &self.distortion {
            None => (x, y),
            Some(d) => {
                let (dx, dy) = d.inverse_displacement_exact(x, y);
                (x + dx, y + dy)
            }
        };
        self.linear_inv.apply(rx, ry)
    }

    pub fn is_flipped(&self) -> bool {
        self.linear.is_flipped()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("PixelMap is always serializable")
    }

    pub fn from_json(s: &str) -> Result<PixelMap, serde_json::Error> {
        let map: PixelMap = serde_json::from_str(s)?;
        if let Some(d) = &map.distortion {
            if !d.is_well_formed() {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "malformed distortion: a polynomial's order must be 2..=4 with one finite coefficient per term, a positive finite scale and an ordered finite domain; a spline needs at least one node, matching weight vectors, a positive finite scale, a non-negative smoothing and an ordered finite domain",
                ));
            }
        }
        let linear_inv = map.linear.inverse().ok_or_else(|| {
            <serde_json::Error as serde::de::Error>::custom("singular linear transform")
        })?;
        Ok(PixelMap {
            linear: map.linear,
            linear_inv,
            distortion: map.distortion,
        })
    }
}

impl InverseMap for PixelMap {
    #[inline]
    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        PixelMap::inverse(self, x, y)
    }

    /// Captures the inverse displacement grid once for the burst (ruling
    /// R-T4-6b) — the whole reason the trait has this method.
    fn inverse_burst(&self) -> Box<dyn Fn(f64, f64) -> (f64, f64) + Send + Sync + '_> {
        let eval = self.inverse_eval();
        Box::new(move |x, y| eval.at(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::linear::{Linear, LinearKind};
    use crate::geometry::polynomial::{Distortion, Polynomial2D, DOMAIN_MARGIN};
    use crate::geometry::ransac::SplitMix64;

    const TW: f64 = 1000.0;
    const TH: f64 = 800.0;

    /// A smooth field no polynomial of order 2..=4 can follow — the one
    /// the registration pin in `align.rs` uses too.
    fn wobble(x: f64, y: f64) -> (f64, f64) {
        (
            1.5 * (x / 300.0).sin() * (y / 250.0).cos(),
            1.5 * (x / 300.0).cos() * (y / 250.0).sin(),
        )
    }

    /// A TPS layer over `n` nodes scattered across the reference frame:
    /// forward = the field, inverse = its negation (an exact-enough
    /// inverse for a field this small, which is all these pins need).
    fn tps_model(seed: u64, n: usize) -> DistortionModel {
        let mut rng = SplitMix64(seed);
        let nodes: Vec<(f64, f64)> = (0..n)
            .map(|_| (rng.next_f64() * TW, rng.next_f64() * TH))
            .collect();
        let mut fx = Vec::new();
        let mut fy = Vec::new();
        for &(x, y) in &nodes {
            let (a, b) = wobble(x, y);
            fx.push(a);
            fy.push(b);
        }
        let ix: Vec<f64> = fx.iter().map(|v| -v).collect();
        let iy: Vec<f64> = fy.iter().map(|v| -v).collect();
        let forward = ThinPlateSpline::fit(&nodes, &fx, &fy, 0.0).expect("forward spline");
        let inverse = ThinPlateSpline::fit(&nodes, &ix, &iy, 0.0).expect("inverse spline");
        let b = forward.node_bounds();
        let (mx, my) = ((b[2] - b[0]) * DOMAIN_MARGIN, (b[3] - b[1]) * DOMAIN_MARGIN);
        DistortionModel::tps(
            forward,
            inverse,
            [b[0] - mx, b[1] - my, b[2] + mx, b[3] + my],
        )
    }

    /// Ruling R-T4-3a/c: the exact path builds NO grid, and each
    /// direction's grid is built only when that direction is asked for
    /// through the PIXEL path. A registered frame's residual statistics
    /// evaluate the map a few hundred times, and the resampler only ever
    /// asks `inverse` while drizzle only asks `forward` — neither should
    /// pay for the other, and neither should be paid at all by a caller
    /// that is not resampling.
    #[test]
    fn only_the_pixel_path_builds_a_grid_and_only_its_own_direction() {
        let _quiet = grid_counters::exclusive();
        let map = PixelMap::with_distortion_model(Linear::identity(), tps_model(24, 60)).unwrap();
        let d = || map.distortion.as_ref().unwrap();
        assert_eq!(d().grids_built(), (false, false), "nothing built yet");

        // The exact path, in both directions, over more points than a
        // frame has inliers: still nothing built.
        for i in 0..1000 {
            let (x, y) = (i as f64 * 0.9, i as f64 * 0.7);
            map.forward_exact(x, y);
            map.inverse_exact(x, y);
            d().forward_displacement_exact(x, y);
            d().inverse_displacement_exact(x, y);
        }
        assert_eq!(
            d().grids_built(),
            (false, false),
            "the exact path must never build a grid"
        );

        // `residual_stats`-shaped use — what registration QA does — goes
        // through `forward_exact`, so it too leaves both unbuilt.
        map.forward_exact(123.0, 456.0);
        assert_eq!(d().grids_built(), (false, false));

        // One inverse PIXEL query builds the inverse grid and only it.
        map.inverse(400.0, 300.0);
        assert_eq!(d().grids_built(), (false, true), "inverse only");
        // …and then the forward one, when something finally asks.
        map.forward(400.0, 300.0);
        assert_eq!(d().grids_built(), (true, true));
    }

    /// Ruling R-T4-6a: a release through ONE clone frees the grid every
    /// other clone sees, and the next pixel request rebuilds it. This is
    /// the whole mechanism the stages lean on — a registered frame's map
    /// lives in five places at once, so a release that only emptied the
    /// handle it was called on would free nothing at all.
    #[test]
    fn releasing_through_one_clone_frees_the_grid_for_every_clone() {
        let _quiet = grid_counters::exclusive();
        let map = PixelMap::with_distortion_model(Linear::identity(), tps_model(26, 60)).unwrap();
        let clone = map.clone();
        map.inverse(100.0, 100.0);
        clone.forward(100.0, 100.0);
        assert_eq!(map.distortion.as_ref().unwrap().grids_built(), (true, true));

        // Released through the CLONE, observed on the original.
        let freed = clone.release_grids();
        assert!(freed > 0, "release must report the bytes it gave back");
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, false),
            "one cache: a release through any handle frees it for all"
        );
        assert_eq!(clone.release_grids(), 0, "releasing twice frees nothing");

        // The values are unchanged after the rebuild — a release is a
        // memory decision, never a numeric one.
        let before = {
            let m2 =
                PixelMap::with_distortion_model(Linear::identity(), tps_model(26, 60)).unwrap();
            m2.inverse(311.0, 222.0)
        };
        let after = map.inverse(311.0, 222.0);
        assert_eq!(before, after);
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, true)
        );

        // A burst taken BEFORE the release keeps working on the grid it
        // captured (ruling R-T4-6b): the handle owns an `Arc`, so the
        // release empties the slot without freeing the data underneath a
        // running band.
        let eval = map.inverse_eval();
        map.release_grids();
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, false)
        );
        assert_eq!(eval.at(311.0, 222.0), after, "the burst survives a release");

        // Linear and polynomial maps have nothing to release.
        assert_eq!(
            PixelMap::linear(Linear::identity())
                .unwrap()
                .release_grids(),
            0
        );
    }

    /// Ruling R-T4-6b: the burst evaluators agree with the one-off pixel
    /// path exactly — they are the same arithmetic with the grid lookup
    /// hoisted, for every arm.
    #[test]
    fn the_burst_evaluators_match_the_one_off_pixel_path() {
        let _quiet = grid_counters::exclusive();
        let linear = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.998, 0.021, -11.5],
                [-0.021, 0.998, 31.25],
                [2.1e-8, -1.0e-8, 1.0],
            ],
        };
        let poly = Distortion {
            order: 2,
            center: (500.0, 400.0),
            scale: 500.0,
            domain: Some([-1.2, -1.2, 1.2, 1.2]),
            forward: Polynomial2D {
                order: 2,
                ax: vec![0.3, -0.2, 0.1],
                ay: vec![-0.1, 0.25, 0.05],
            },
            inverse: Polynomial2D {
                order: 2,
                ax: vec![-0.3, 0.2, -0.1],
                ay: vec![0.1, -0.25, -0.05],
            },
        };
        let maps = [
            PixelMap::linear(linear).unwrap(),
            PixelMap::with_distortion(linear, poly).unwrap(),
            PixelMap::with_distortion_model(linear, tps_model(27, 80)).unwrap(),
        ];
        let mut rng = SplitMix64(31337);
        for map in &maps {
            let fwd = map.forward_eval();
            let inv = map.inverse_eval();
            for _ in 0..200 {
                let (x, y) = (rng.next_f64() * TW, rng.next_f64() * TH);
                assert_eq!(fwd.at(x, y), map.forward(x, y));
                assert_eq!(inv.at(x, y), map.inverse(x, y));
                // `InverseMap::inverse_burst` is the trait-object route
                // the resampler takes; same numbers.
                let burst = map.inverse_burst();
                assert_eq!(burst(x, y), map.inverse(x, y));
            }
        }
    }

    /// Ruling R-T4-3b: a clone SHARES the grids rather than copying them.
    /// A registered frame's map is cloned into run state several times
    /// over (`rc.measured`, `GroupMember`, `StackFrame`,
    /// `RegisteredFrame`, `RegisteredSource`), and `Clone` on a bare
    /// `OnceLock<TpsGrid>` copies the built grid — at ~8.6 MB a direction
    /// on a full-frame map that was gigabytes of duplicate.
    #[test]
    fn a_clone_shares_the_grids_it_does_not_copy_them() {
        let _quiet = grid_counters::exclusive();
        let map = PixelMap::with_distortion_model(Linear::identity(), tps_model(25, 60)).unwrap();
        map.inverse(100.0, 100.0);
        let built = map.distortion.as_ref().unwrap().grids_built();
        assert_eq!(built, (false, true));

        let clone = map.clone();
        let (a, b) = (
            map.distortion.as_ref().unwrap().grid_cache().unwrap(),
            clone.distortion.as_ref().unwrap().grid_cache().unwrap(),
        );
        assert!(
            Arc::ptr_eq(a, b),
            "a clone must share ONE grid cache allocation"
        );
        // The clone therefore already has the built grid — no rebuild —
        // and building the other direction on the clone is visible on the
        // original, because it is the same cell.
        assert_eq!(
            clone.distortion.as_ref().unwrap().grids_built(),
            (false, true)
        );
        clone.forward(100.0, 100.0);
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (true, true),
            "one cache, both handles"
        );
        // And they still compare equal: the cache is not identity.
        assert_eq!(map, clone);
    }

    /// Step 2: the 8-px bilinear grid the pixel path reads agrees with the
    /// exact spline. A registration residual field is smooth on the scale
    /// of 8 px, so the bilinear error is `~h²/8 · |f''|` — parts in ten
    /// thousand of a pixel, not hundredths.
    #[test]
    fn the_tps_grid_agrees_with_the_exact_spline() {
        let _quiet = grid_counters::exclusive();
        let model = tps_model(21, 200);
        let mut rng = SplitMix64(555);
        let mut worst = 0.0f64;
        for _ in 0..500 {
            let (x, y) = (rng.next_f64() * TW, rng.next_f64() * TH);
            let (gx, gy) = model.forward_displacement(x, y);
            let (ex, ey) = model.forward_displacement_exact(x, y);
            worst = worst.max((gx - ex).abs()).max((gy - ey).abs());
            let (gx, gy) = model.inverse_displacement(x, y);
            let (ex, ey) = model.inverse_displacement_exact(x, y);
            worst = worst.max((gx - ex).abs()).max((gy - ey).abs());
        }
        assert!(worst < 0.02, "grid vs exact spline: worst {worst} px");
        // Outside the fitted domain both paths clamp to the same edge, so
        // the grid and the exact spline agree there too — not just inside.
        for (x, y) in [(-5000.0, -5000.0), (9e4, 9e4), (-1.0, TH + 1.0)] {
            let (gx, gy) = model.inverse_displacement(x, y);
            let (ex, ey) = model.inverse_displacement_exact(x, y);
            assert!(
                (gx - ex).abs() < 0.02 && (gy - ey).abs() < 0.02,
                "outside the domain at ({x}, {y}): grid ({gx}, {gy}) vs exact ({ex}, {ey})"
            );
        }
    }

    /// Step 2: a `Tps` map's JSON keeps the splines (to `serde_json`'s own
    /// one-ULP float parse — see
    /// `tps::tests::a_spline_round_trips_through_json`) and the grid is
    /// rebuilt on the far side, never stored.
    #[test]
    fn a_tps_map_round_trips_and_rebuilds_its_grid() {
        let _quiet = grid_counters::exclusive();
        let linear = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.999, 0.013, -7.5],
                [-0.013, 0.999, 22.25],
                [1.1e-8, -3.0e-8, 1.0],
            ],
        };
        let map = PixelMap::with_distortion_model(linear, tps_model(22, 120)).unwrap();
        // Evaluate first, so the source map carries a BUILT grid: equality
        // and the JSON must both ignore it. Against a bit-identical map
        // that has NOT been evaluated, equality is exact.
        let before = map.forward(511.0, 407.0);
        let fresh = PixelMap::with_distortion_model(linear, tps_model(22, 120)).unwrap();
        assert_eq!(map, fresh, "the cached grid is not part of identity");

        let json = map.to_json();
        assert!(json.contains("\"kind\":\"tps\""), "{json}");
        assert!(!json.contains("grid"), "the grid must never be serialized");
        let back = PixelMap::from_json(&json).unwrap();
        match back.distortion.as_ref().unwrap() {
            DistortionModel::Tps {
                forward, domain, ..
            } => {
                assert_eq!(forward.nodes.len(), 120);
                assert_eq!(domain.len(), 4);
            }
            other => panic!("expected a spline: {other:?}"),
        }
        let after = back.forward(511.0, 407.0);
        assert!(
            (before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9,
            "{before:?} vs {after:?}"
        );
        // Forward then inverse returns the subject pixel: the two splines
        // are independent fits, so this is not an algebraic identity — it
        // is the 0.1 px-class agreement a registered frame relies on.
        let (u, v) = back.forward(311.0, 222.0);
        let (x, y) = back.inverse(u, v);
        assert!(
            (x - 311.0).abs() < 0.05 && (y - 222.0).abs() < 0.05,
            "round trip landed at ({x}, {y})"
        );
    }

    /// Ruling R-M4c-6: the tag defaults to `polynomial`, so every
    /// `transform_json` written before M4c still decodes — here as the
    /// literal fixture string an M1 row carries — and an unknown kind is
    /// refused loudly rather than silently treated as "no distortion".
    #[test]
    fn an_untagged_distortion_is_a_polynomial_and_an_unknown_kind_is_refused() {
        const M1_ROW: &str = "{\"linear\":{\"kind\":\"affine\",\"m\":[[1.0,0.0,3.5],[0.0,1.0,-2.5],[0.0,0.0,1.0]]},\
             \"linearInv\":{\"kind\":\"affine\",\"m\":[[1.0,0.0,-3.5],[0.0,1.0,2.5],[0.0,0.0,1.0]]},\
             \"distortion\":{\"order\":2,\"center\":[500.0,400.0],\"scale\":500.0,\
             \"domain\":[-1.1,-1.1,1.1,1.1],\
             \"forward\":{\"order\":2,\"ax\":[0.1,0.2,0.3],\"ay\":[0.4,0.5,0.6]},\
             \"inverse\":{\"order\":2,\"ax\":[-0.1,-0.2,-0.3],\"ay\":[-0.4,-0.5,-0.6]}}}";
        let map = PixelMap::from_json(M1_ROW).expect("an M1 transform_json still decodes");
        match map.distortion.as_ref().unwrap() {
            DistortionModel::Polynomial(d) => {
                assert_eq!(d.order, 2);
                assert_eq!(d.center, (500.0, 400.0));
            }
            other => panic!("expected a polynomial: {other:?}"),
        }
        assert_eq!(map.distortion.as_ref().unwrap().label(), "polynomial2");
        // The same row with an explicit tag is the same map — these are
        // short decimals, so equality here IS exact.
        let tagged = M1_ROW.replace(
            "\"distortion\":{\"order\":2",
            "\"distortion\":{\"kind\":\"polynomial\",\"order\":2",
        );
        assert_ne!(tagged, M1_ROW, "the splice must have matched");
        assert_eq!(PixelMap::from_json(&tagged).unwrap(), map);
        // An unknown kind is an error, not a shrug.
        let alien = M1_ROW.replace(
            "\"distortion\":{\"order\":2",
            "\"distortion\":{\"kind\":\"wavelet\",\"order\":2",
        );
        let err = PixelMap::from_json(&alien).unwrap_err().to_string();
        assert!(err.contains("unknown distortion kind"), "{err}");
    }

    /// A malformed spline is refused by `from_json` the same way a
    /// malformed polynomial is.
    #[test]
    fn from_json_rejects_a_malformed_spline() {
        let map = PixelMap::with_distortion_model(Linear::identity(), tps_model(23, 40)).unwrap();
        let good = map.to_json();
        // An inverted domain.
        let inverted = good.replace(
            "\"domain\":[",
            "\"domain\":[9e9,9e9,-9e9,-9e9],\"unused\":[",
        );
        assert_ne!(inverted, good, "the splice must have matched");
        assert!(PixelMap::from_json(&inverted).is_err());
        // A weight vector that does not match the node list.
        let wx_at = good.find("\"wx\":[").expect("wx in the json");
        let wx_end = good[wx_at..].find("],").expect("wx closes") + wx_at + 2;
        let short = format!("{}\"wx\":[1.0],{}", &good[..wx_at], &good[wx_end..]);
        assert_ne!(short, good, "the splice must have matched");
        assert!(PixelMap::from_json(&short).is_err(), "{short}");
    }

    #[test]
    fn linear_map_round_trips_and_serializes() {
        let l = Linear {
            kind: LinearKind::Homography,
            m: [
                [0.992, 0.0387, -23.54],
                [-0.0383, 0.9922, 136.37],
                [8.8e-8, -2.5e-8, 1.0],
            ],
        };
        let map = PixelMap::linear(l).unwrap();
        let (u, v) = map.forward(1234.5, 678.25);
        let (x, y) = map.inverse(u, v);
        assert!((x - 1234.5).abs() < 1e-9 && (y - 678.25).abs() < 1e-9);
        let json = map.to_json();
        assert!(json.contains("\"kind\":\"homography\""));
        assert!(json.contains("\"distortion\":null"));
        let back = PixelMap::from_json(&json).unwrap();
        let (u2, v2) = back.forward(1234.5, 678.25);
        assert!((u - u2).abs() < 1e-12 && (v - v2).abs() < 1e-12);
    }

    #[test]
    fn singular_linear_is_rejected() {
        let l = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 2.0, 0.0], [2.0, 4.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(PixelMap::linear(l).is_none());
    }

    #[test]
    fn identity_linear_implements_inverse_map() {
        let id = Linear::identity();
        let m: &dyn InverseMap = &id;
        assert_eq!(m.inverse(3.0, 4.0), (3.0, 4.0));
    }

    #[test]
    fn from_json_rejects_a_malformed_distortion() {
        let l = Linear::identity();
        let good = PixelMap::linear(l).unwrap().to_json();
        // Splice in a distortion whose order is out of range and whose
        // coefficient vectors are too short for it.
        let bad = good.replace(
            "\"distortion\":null",
            "\"distortion\":{\"order\":5,\"center\":[0.0,0.0],\"scale\":1.0,\
             \"forward\":{\"order\":5,\"ax\":[1.0],\"ay\":[1.0]},\
             \"inverse\":{\"order\":5,\"ax\":[1.0],\"ay\":[1.0]}}",
        );
        assert_ne!(good, bad, "the splice must have matched");
        assert!(PixelMap::from_json(&bad).is_err());
        // A well-formed distortion round-trips.
        let d = Distortion {
            order: 2,
            center: (10.0, 20.0),
            scale: 100.0,
            domain: Some([-1.0, -1.0, 1.0, 1.0]),
            forward: Polynomial2D {
                order: 2,
                ax: vec![0.1, 0.2, 0.3],
                ay: vec![0.4, 0.5, 0.6],
            },
            inverse: Polynomial2D {
                order: 2,
                ax: vec![-0.1, -0.2, -0.3],
                ay: vec![-0.4, -0.5, -0.6],
            },
        };
        assert!(d.is_well_formed());
        let map = PixelMap::with_distortion(l, d).unwrap();
        let back = PixelMap::from_json(&map.to_json()).unwrap();
        assert_eq!(map, back);
    }

    #[test]
    fn from_json_recomputes_a_corrupted_stored_inverse() {
        let linear = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 12.5], [0.0, 1.0, -7.25], [0.0, 0.0, 1.0]],
        };
        let map = PixelMap::linear(linear).unwrap();
        let good = map.to_json();
        let stored_inv = format!(
            "\"linearInv\":{}",
            serde_json::to_string(&map.linear_inv).unwrap()
        );
        assert!(
            good.contains(&stored_inv),
            "expected to find the stored inverse in the JSON"
        );
        // Splice in a wrong-but-well-formed inverse (a similarity that scales
        // by 2) — a stale/hand-edited inverse must never survive `from_json`.
        let corrupted = good.replace(
            &stored_inv,
            "\"linearInv\":{\"kind\":\"similarity\",\"m\":[[2.0,0.0,0.0],[0.0,2.0,0.0],[0.0,0.0,1.0]]}",
        );
        assert_ne!(good, corrupted, "the splice must have matched");
        let back = PixelMap::from_json(&corrupted).unwrap();
        assert_eq!(back.linear_inv, linear.inverse().unwrap());
    }

    #[test]
    fn distortion_json_without_a_domain_is_unbounded_and_an_inverted_domain_is_rejected() {
        let l = Linear::identity();
        let good = PixelMap::linear(l).unwrap().to_json();
        // A transform_json written before the domain field existed.
        let old = good.replace(
            "\"distortion\":null",
            "\"distortion\":{\"order\":2,\"center\":[0.0,0.0],\"scale\":1.0,\
             \"forward\":{\"order\":2,\"ax\":[0.0,0.0,0.0],\"ay\":[0.0,0.0,0.0]},\
             \"inverse\":{\"order\":2,\"ax\":[0.0,0.0,0.0],\"ay\":[0.0,0.0,0.0]}}",
        );
        assert_ne!(good, old, "the splice must have matched");
        let map = PixelMap::from_json(&old).unwrap();
        match map.distortion.as_ref().unwrap() {
            DistortionModel::Polynomial(d) => assert_eq!(d.domain, None),
            other => panic!("an untagged distortion must decode as a polynomial: {other:?}"),
        }
        assert_eq!(map.inverse(3.0, 4.0), (3.0, 4.0));
        let inverted = old.replace(
            "\"scale\":1.0,",
            "\"scale\":1.0,\"domain\":[1.0,0.0,-1.0,0.0],",
        );
        assert_ne!(old, inverted, "the splice must have matched");
        assert!(PixelMap::from_json(&inverted).is_err());
    }
}
