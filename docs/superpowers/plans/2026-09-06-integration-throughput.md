# Integration Throughput Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make master integration read at the speed of the disk it is reading from — 241.6 s → ≤ 40 s on the profiled 100-frame bias set — without staging copies, without a second in-memory copy of the data, and with the operator able to see what the build is doing.

**Architecture:** Three changes to the banded reader, in order of measured value. (1) The 256 MiB compile-time band budget becomes a policy resolved from physical RAM, container limits and the compute-queue concurrency, with an operator override. (2) Band buffers hold the source's own bytes instead of widened `f32`, halving band memory for BITPIX 16 files and so doubling rows per band for free. (3) Reads become positional (`pread`) and run in parallel across frames on the shared rayon pool. Read/compute overlap is deliberately NOT added — the measurement says the cure for the alternation is fewer bands, not a second buffer.

**Tech Stack:** Rust (`athenaeum-core`), rayon, `libc` (unix, already a dependency), `windows-sys` (new, cfg(windows) only, already in `Cargo.lock` transitively), React/TS frontend, Tauri + Axum command mirror.

**Spec:** `docs/superpowers/specs/2026-09-06-integration-throughput-design.md`
**Research:** `docs/superpowers/research/2026-09-06-master-integration-io-profiling.md`

## Global Constraints

- **Master pixels must stay byte-identical** for the same inputs. Every task that touches the read or combine path proves it (Task 4 carries the explicit fixture; Tasks 3, 5 and 6 must not change any value).
- **No staging copies, no cache, no second in-memory copy of the frame data.** This is an explicit owner instruction. Growing an allocation the code already makes is in scope; adding one that mirrors the data is not.
- **Two backends in sync.** Any new Tauri command gets its Axum mirror in the same change (`crates/athenaeum-web/src/routes/<same_domain>.rs`), with the logic in `athenaeum-core/src/api/`.
- **Serde boundary:** `MasterBuildProgressEvent` has no `#[serde(rename_all)]` and its TS mirror uses snake_case. Do not add a rename to it.
- **Never swallow errors.** Log before returning at every boundary.
- **Design tokens, not raw colors** in any frontend change (`text-content-muted`, `bg-accent`, …).
- **Release gates for this cycle:** `cargo build --workspace`, `cargo test --workspace`, `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`.
- **Do not name other codebases** in code, comments or function names.
- Commit as the user (`eg013ra1n` / `vilen.sharifov@gmail.com`), on `main`.

## Measurement protocol (used by every acceptance gate)

The page cache must be evicted before each cold run — `purge(8)` is refused to non-root users and `F_NOCACHE` proved unreliable on APFS (research §2). Stream more unrelated data than the machine has RAM:

```bash
find /Volumes/bigbase2/Astrobase/Calibration/dark6200 -name '*.fit*' \
  | head -400 | tr '\n' '\0' | xargs -0 -n4 cat | wc -c   # 25.7 GB, ~106 s
```

Then:

```bash
BIAS="/Volumes/bigbase2/Astrobase/Calibration/ASI2600MC DUO/ASI2600MC DUO/2024/2024-09-21/BIAS"
cargo run --release -p athenaeum-core --example band_profile -- "$BIAS" "" 256
```

Baselines to beat, all cold, all on the profiling machine (16 GB, WD8005FFBX 7200 rpm SATA):

| set | shipping today |
| ---- | ---- |
| 100-frame bias, 5.2 GB | 241.6 s (read 235.5 s @ 22 MB/s, combine 6.0 s) |
| 30-frame dark, 1.56 GB | 11.8 s (read 9.07 s @ 173 MB/s, combine 2.70 s) |
| drive ceiling, whole-file sequential | 243 MB/s |

---

## File Structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/examples/band_profile.rs` | **new** — checked-in profiling harness; the gate for every task below |
| `crates/athenaeum-core/src/integration/band_budget.rs` | **new** — RAM probe (incl. cgroup), auto formula, setting resolution |
| `crates/athenaeum-core/src/integration/banded.rs` | `BandSource` + **new** `BandPlanes`/`PlaneKind`; positional parallel reads; budget→rows is now a method |
| `crates/athenaeum-core/src/integration/engine.rs` | band budget becomes a parameter; `IntegrationOutput` gains read/combine timings; gather reads through `BandPlanes` |
| `crates/athenaeum-core/src/integration/mod.rs` | declares `band_budget` |
| `crates/athenaeum-core/src/settings/mod.rs` | `integration.band_budget_mb` key, default, clamped getter |
| `crates/athenaeum-core/src/api/masters.rs` | resolves the budget, passes it to the engine, logs the build lifecycle |
| `crates/athenaeum-core/src/api/compute.rs` | `get_integration_band_budget` / `set_integration_band_budget` handlers |
| `crates/athenaeum-core/src/calibration_library/light_cal.rs`, `cosmetic.rs` | migrate to `BandPlanes`; keep the 256 MiB floor budget |
| `crates/athenaeum-tauri/src/commands/compute.rs`, `crates/athenaeum-web/src/routes/compute.rs` | the two command mirrors |
| `src/components/CalibrationHierarchyView.tsx`, `src/components/calibration/CalibrationTableView.tsx` | stop discarding `percent` |
| `src/pages/Settings.tsx` | the budget control on the Calibration tab |

---

### Task 1: Timing instrumentation and the profiling harness

Nothing below can be judged without a repeatable measurement, so this comes first. It also adds the `band_budget_bytes` parameter to the two public integration entry points — passing today's constant, so behaviour is unchanged — which is what lets the harness sweep budgets.

**Files:**
- Create: `crates/athenaeum-core/examples/band_profile.rs`
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (`IntegrationOutput`, `run_banded`, `integrate_bias_like`, `integrate_flat`, `integrate_flat_inner`, tests)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (two call sites gain the argument)

**Interfaces:**
- Produces:
  - `IntegrationOutput { .., read_duration: Duration, combine_duration: Duration, band_rows: usize, bands: usize, bytes_read: u64 }` — the engine is the only place that knows the band geometry and the exact number of pixel bytes it pulled, so it reports them rather than leaving callers to re-derive or guess from file sizes.
  - `EngineProgress { on_band: &'a dyn Fn(usize, usize, u64, u64) }` — `(band_index_1based, bands_total, bytes_read_so_far, bytes_total)`. Task 7 puts the byte pair on the progress event; band counts alone say nothing about size once bands are machine-sized.
  - `integrate_bias_like(paths, recipe, pool, scratch_dir, cancel, progress, band_budget_bytes)` and `integrate_flat(paths, precal, recipe, pool, scratch_dir, cancel, progress, band_budget_bytes)` — both take the budget as their last argument.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block at the bottom of `crates/athenaeum-core/src/integration/engine.rs` (the helpers `write`, `nop`, `pool` already exist there):

```rust
    /// The harness and, from Task 7, the build's completion log line report
    /// where the time went. Both numbers come out of the engine because only
    /// it can separate the two phases.
    #[test]
    fn integration_output_reports_read_and_combine_time() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "t1.fits", w, h, |_, _| 10.0),
            write(dir.path(), "t2.fits", w, h, |_, _| 20.0),
            write(dir.path(), "t3.fits", w, h, |_, _| 30.0),
        ];
        let on_band = nop();
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            BAND_BUDGET_BYTES,
        )
        .unwrap();
        assert!(out.read_duration > std::time::Duration::ZERO, "read time not recorded");
        assert!(out.combine_duration > std::time::Duration::ZERO, "combine time not recorded");
        assert!(out.data.iter().all(|&v| v == 20.0), "median unchanged by instrumentation");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core integration_output_reports_read_and_combine_time`
Expected: FAIL to compile — `integrate_bias_like` takes 6 arguments, and `IntegrationOutput` has no field `read_duration`.

- [ ] **Step 3: Add the fields and the parameter**

In `IntegrationOutput` (after `all_bad_pixels`):

```rust
    /// Wall time spent inside `BandSource::read_band` across every band,
    /// including a flat's pass-1 reads. Separated from `combine_duration`
    /// because the two phases have completely different bottlenecks and only
    /// the engine can tell them apart.
    pub read_duration: std::time::Duration,
    /// Wall time spent in the parallel per-pixel combine across every band.
    pub combine_duration: std::time::Duration,
    /// Rows per band and how many bands the run used — the two numbers the
    /// band budget actually decides, reported so the build's log line does not
    /// have to re-derive them.
    pub band_rows: usize,
    pub bands: usize,
    /// Pixel bytes actually read (sum of `rows * width * bytes_per_sample`
    /// over every band and frame). Not the files' size on disk: headers and
    /// padding are never read, and a flat's pass 1 reads only its central
    /// third.
    pub bytes_read: u64,
```

Widen the progress callback in the same edit, since Task 7 needs the byte pair
and the harness written below is its first caller:

```rust
pub struct EngineProgress<'a> {
    /// `(band_index_1based, bands_total, bytes_read_so_far, bytes_total)`.
    pub on_band: &'a dyn Fn(usize, usize, u64, u64),
}
```

`bytes_total` is `h * per_row_bytes` for a bias-like run and, for a flat, pass
1's central-third rows plus pass 2's full height — compute it once next to
`band_rows` and capture it in the closure. Every existing caller updates: the
`nop()` test helper in `engine.rs`, the `on_band` closure in
`api/masters.rs:926`, and the harness below.

In `run_banded`, before the band loop:

```rust
    let mut read_duration = std::time::Duration::ZERO;
    let mut combine_duration = std::time::Duration::ZERO;
```

Wrap the two phases inside the loop:

```rust
        let rows = band_rows.min(h - y0);
        let t_read = std::time::Instant::now();
        src.read_band(y0, rows, &mut band_bufs)?;
        read_duration += t_read.elapsed();

        let t_combine = std::time::Instant::now();
        let out_band = &mut out[y0 * w..(y0 + rows) * w];
        pool.install(|| {
            // ... unchanged ...
        });
        combine_duration += t_combine.elapsed();
```

Accumulate `bytes_read` in the same loop (`rows * w * per_row_bytes` per band)
and set `band_rows` / `bands` from the values already computed above it. Add all
five to the returned struct literal. Then change the two public wrappers to take `band_budget_bytes: usize` as their last parameter and forward it instead of `BAND_BUDGET_BYTES`:

```rust
#[allow(clippy::too_many_arguments)]
pub fn integrate_bias_like(
    paths: &[PathBuf],
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_bias_like_inner(paths, recipe, pool, scratch_dir, cancel, progress, band_budget_bytes)
}
```

and the same shape for `integrate_flat`. In `integrate_flat_inner`, time pass 1 too and fold it in:

```rust
    let mut pass1_read = std::time::Duration::ZERO;
    // ... inside the `while y < cy1` loop:
        let t_read = std::time::Instant::now();
        src.read_band(y, rows, &mut band_bufs)?;
        pass1_read += t_read.elapsed();
    // ... after run_banded returns:
    out.read_duration += pass1_read;
```

- [ ] **Step 4: Fix the existing call sites**

`crates/athenaeum-core/src/api/masters.rs` — both engine calls gain `BAND_BUDGET_BYTES` as the last argument (it is `pub(crate)`, same crate):

```rust
        integrate_flat(
            &paths, &precal, resolved_combine, pool, &scratch,
            cancel_flag.as_ref(), progress,
            crate::integration::engine::BAND_BUDGET_BYTES,
        )?
```

and likewise `integrate_bias_like`. In `engine.rs`'s own tests, `negatives_pass_through_unclipped` and every other caller of the public `integrate_bias_like` / `integrate_flat` gains `BAND_BUDGET_BYTES`. Calls to `integrate_flat_inner` already pass a budget and are untouched.

- [ ] **Step 5: Run the test**

Run: `cargo test -p athenaeum-core --lib integration::`
Expected: PASS, including `integration_output_reports_read_and_combine_time` and every pre-existing engine test.

- [ ] **Step 6: Write the harness**

Create `crates/athenaeum-core/examples/band_profile.rs`:

```rust
//! Profiling harness for the banded integration engine — the measurement gate
//! for `docs/superpowers/plans/2026-09-06-integration-throughput.md`.
//!
//! Usage:
//!     cargo run --release -p athenaeum-core --example band_profile -- <dir> [name-substring] [budget_mb] [threads]
//!
//! `budget_mb` defaults to 0, meaning "resolve the budget the way the app
//! does"; pass a number to force one. `threads` 0 means "all cores".
//!
//! COLD RUNS ONLY MEAN ANYTHING. `purge(8)` is refused to non-root users and
//! `F_NOCACHE` is unreliable on APFS (it reported 103 MB/s and 252 MB/s for
//! the same configuration on consecutive runs). Evict by streaming more
//! unrelated data than the machine has RAM before each run, e.g.
//!
//!     find <some other 25 GB of files> -name '*.fit*' | head -400 \
//!       | tr '\n' '\0' | xargs -0 -n4 cat | wc -c
use athenaeum_core::integration::combine::{IntegrationRecipe, Rejection};
use athenaeum_core::integration::engine::{integrate_bias_like, EngineProgress};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: band_profile <dir> [name-substring] [budget_mb] [threads]");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[1]);
    let pat = args.get(2).cloned().unwrap_or_default();
    let budget_mb: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);
    let threads: usize = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(0);

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "fits" || e == "fit").unwrap_or(false))
        .filter(|p| pat.is_empty() || p.file_name().unwrap().to_string_lossy().contains(&pat))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no frames matched");

    let budget = if budget_mb == 0 {
        athenaeum_core::integration::band_budget::auto_budget_bytes()
    } else {
        budget_mb * 1024 * 1024
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let bytes_on_disk: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();

    println!(
        "{} frames, {:.2} GB on disk, budget {} MiB, {} threads",
        paths.len(),
        bytes_on_disk as f64 / 1e9,
        budget / (1024 * 1024),
        pool.current_num_threads()
    );

    let on_band = |cur: usize, total: usize, _done: u64, _all: u64| {
        if cur == 1 { println!("bands: {total}"); }
    };
    let t = Instant::now();
    let out = integrate_bias_like(
        &paths,
        IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        &pool,
        &std::env::temp_dir(),
        &AtomicBool::new(false),
        EngineProgress { on_band: &on_band },
        budget,
    )
    .expect("integration failed");
    let all = t.elapsed();

    let read_s = out.read_duration.as_secs_f64();
    println!("bands     {:>9}   ({} rows each)", out.bands, out.band_rows);
    println!("read      {:>9.2?}   {:>6.0} MB/s", out.read_duration, out.bytes_read as f64 / read_s / 1e6);
    println!("combine   {:>9.2?}", out.combine_duration);
    println!(
        "TOTAL     {:>9.2?}   read {:.0}%  combine {:.0}%",
        all,
        100.0 * read_s / all.as_secs_f64(),
        100.0 * out.combine_duration.as_secs_f64() / all.as_secs_f64()
    );
    // Fingerprint so later tasks can prove the pixels did not move.
    let sum: f64 = out.data.iter().map(|&v| v as f64).sum();
    println!("checksum  {:.6e}  ({}x{})", sum, out.width, out.height);
}
```

The `auto_budget_bytes()` call does not exist yet — Task 2 adds it. Until then, hardcode `256 * 1024 * 1024` on that branch and replace it in Task 2 Step 6.

- [ ] **Step 7: Record the baseline**

Run the eviction command from the measurement protocol, then:

```bash
BIAS="/Volumes/bigbase2/Astrobase/Calibration/ASI2600MC DUO/ASI2600MC DUO/2024/2024-09-21/BIAS"
cargo run --release -p athenaeum-core --example band_profile -- "$BIAS" "" 256
```

Expected: ~240 s total, read ~22 MB/s, 40 bands. **Write the printed `checksum` line into the task's commit message** — Tasks 3, 4 and 5 must reproduce it exactly.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/examples/band_profile.rs \
        crates/athenaeum-core/src/integration/engine.rs \
        crates/athenaeum-core/src/api/masters.rs
git commit -m "perf(integration): time the read and combine phases; add the profiling harness"
```

---

### Task 2: The band-budget policy

**Files:**
- Create: `crates/athenaeum-core/src/integration/band_budget.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs`
- Modify: `crates/athenaeum-core/src/settings/mod.rs`
- Modify: `crates/athenaeum-core/Cargo.toml`
- Modify: `crates/athenaeum-core/examples/band_profile.rs` (swap the hardcoded fallback for the real call)

**Interfaces:**
- Consumes: `SettingsManager::get_with_precedence`, `SettingsManager::get_compute_max_concurrent` (both already exist in `settings/mod.rs`).
- Produces:
  - `band_budget::MIN_BUDGET_BYTES: usize` (256 MiB), `MAX_BUDGET_BYTES` (8 GiB), `FALLBACK_BUDGET_BYTES` (1 GiB)
  - `band_budget::total_ram_bytes() -> Option<u64>`
  - `band_budget::auto_budget_bytes() -> usize`
  - `band_budget::resolve_budget_bytes(conn: &rusqlite::Connection, settings: &SettingsManager) -> anyhow::Result<usize>`
  - `band_budget::parse_cgroup_limit(text: &str) -> Option<u64>` (pub(crate), for the test)
  - `settings::keys::INTEGRATION_BAND_BUDGET_MB = "integration.band_budget_mb"`, `settings::defaults::INTEGRATION_BAND_BUDGET_MB = "0"`
  - `SettingsManager::get_integration_band_budget_mb(&self, conn) -> anyhow::Result<usize>`

- [ ] **Step 1: Write the failing tests**

Create `crates/athenaeum-core/src/integration/band_budget.rs` with only its `mod tests` block for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_budget_is_a_quarter_of_ram_within_bounds() {
        let b = auto_budget_bytes();
        assert!(b >= MIN_BUDGET_BYTES, "auto {b} below the floor — would be slower than the old constant");
        assert!(b <= MAX_BUDGET_BYTES, "auto {b} above the cap");
        if let Some(ram) = total_ram_bytes() {
            let want = (ram / 4).clamp(MIN_BUDGET_BYTES as u64, MAX_BUDGET_BYTES as u64) as usize;
            assert_eq!(b, want, "auto must be a quarter of {ram} bytes, clamped");
        }
    }

    #[test]
    fn cgroup_v2_limit_parses_and_max_means_unlimited() {
        assert_eq!(parse_cgroup_limit("2147483648\n"), Some(2_147_483_648));
        assert_eq!(parse_cgroup_limit("max\n"), None, "'max' means no limit, not a limit of zero");
        assert_eq!(parse_cgroup_limit(""), None);
        assert_eq!(parse_cgroup_limit("not a number"), None);
        // cgroup v1 writes a sentinel near u64::MAX for "unlimited"; anything
        // that large is not a real container limit.
        assert_eq!(parse_cgroup_limit("9223372036854771712"), None);
    }

    #[test]
    fn configured_value_is_clamped_and_zero_means_auto() {
        assert_eq!(clamp_configured_mb(0), None, "0 is the auto sentinel, not a size");
        assert_eq!(clamp_configured_mb(1), Some(64), "clamps UP to the 64 MB floor");
        assert_eq!(clamp_configured_mb(512), Some(512));
        assert_eq!(clamp_configured_mb(999_999), Some(16384), "clamps DOWN to the 16 GB cap");
    }

    #[test]
    fn concurrency_divides_the_budget_but_never_below_the_floor() {
        assert_eq!(per_job_budget(4 * 1024 * 1024 * 1024, 1), 4 * 1024 * 1024 * 1024);
        assert_eq!(per_job_budget(4 * 1024 * 1024 * 1024, 4), 1024 * 1024 * 1024);
        assert_eq!(
            per_job_budget(512 * 1024 * 1024, 8),
            MIN_BUDGET_BYTES,
            "two admitted builds must not each claim a quarter of RAM, but neither may drop below the old constant"
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core band_budget`
Expected: FAIL — the module is not declared and none of the items exist.

- [ ] **Step 3: Implement the module**

Prepend to `crates/athenaeum-core/src/integration/band_budget.rs`:

```rust
//! Resolves the working-memory budget for banded integration.
//!
//! Until 2026-09-06 this was a compile-time `256 * 1024 * 1024`. Profiling
//! (`docs/superpowers/research/2026-09-06-master-integration-io-profiling.md`)
//! measured that constant costing 5.3x on a 100-frame set: it yields 105-row
//! bands, so the reader crosses all 100 files forty times and gets 22 MB/s off
//! a drive that sustains 243 MB/s. The budget is a property of the machine,
//! so it is resolved from the machine.

use anyhow::Result;
use rusqlite::Connection;

use crate::settings::{defaults, keys, SettingsManager};

/// The pre-2026-09-06 constant. The policy is floored here so it can never
/// make any machine slower than it already is.
pub const MIN_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// 100 frames of 26 Mpx as `u16` is 5.2 GB; 8 GiB is where a large machine
/// reaches a single band and therefore whole-file sequential reads. Above
/// that the budget buys nothing an integration can spend.
pub const MAX_BUDGET_BYTES: usize = 8 * 1024 * 1024 * 1024;

/// Used when the RAM probe fails. Measured at 101.2 s against the old
/// constant's 241.6 s on the profiled set, and safe on any machine with the
/// 8 GB a 26 Mpx pipeline already needs.
pub const FALLBACK_BUDGET_BYTES: usize = 1024 * 1024 * 1024;

/// Bounds for an explicitly configured `integration.band_budget_mb`.
const CONFIGURED_MIN_MB: usize = 64;
const CONFIGURED_MAX_MB: usize = 16384;

/// Physical RAM this process may actually use, in bytes.
///
/// On Linux this is `min(MemTotal, container limit)` — **load-bearing for the
/// Docker/web build**: `/proc/meminfo` reports the HOST's RAM inside a
/// container, so without the cgroup read a 2 GB container would size an 8 GiB
/// budget and be OOM-killed.
pub fn total_ram_bytes() -> Option<u64> {
    platform_total_ram()
}

#[cfg(target_os = "linux")]
fn platform_total_ram() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            total = Some(kb.saturating_mul(1024));
            break;
        }
    }
    let total = total?;
    let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|s| parse_cgroup_limit(&s))
        .or_else(|| {
            std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
                .ok()
                .and_then(|s| parse_cgroup_limit(&s))
        });
    Some(match limit {
        Some(l) => total.min(l),
        None => total,
    })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_total_ram() -> Option<u64> {
    let mut size: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    // SAFETY: `name` is NUL-terminated, `size`/`len` are correctly sized and
    // owned by this frame, and the new-value pointer is null (a pure read).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr() as *const libc::c_char,
            &mut size as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && size > 0 { Some(size) } else { None }
}

#[cfg(windows)]
fn platform_total_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is a plain POD struct; zeroing it and stamping
    // dwLength is exactly the documented calling convention.
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 && st.ullTotalPhys > 0 {
        Some(st.ullTotalPhys)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_total_ram() -> Option<u64> {
    None
}

/// Parse one cgroup memory-limit file. `None` means "no limit here": cgroup v2
/// writes the literal `max`, and v1 writes a sentinel near `u64::MAX` — a
/// number that large is not a container limit, it is the absence of one.
pub(crate) fn parse_cgroup_limit(text: &str) -> Option<u64> {
    const UNLIMITED_FLOOR: u64 = 1 << 62;
    let v: u64 = text.trim().parse().ok()?;
    if v == 0 || v >= UNLIMITED_FLOOR { None } else { Some(v) }
}

/// A quarter of physical RAM, clamped. A quarter and not a half because the
/// same process also holds the catalog, the render pipeline and the transfer
/// store — and because the OS needs page cache for the very files being read.
pub fn auto_budget_bytes() -> usize {
    match total_ram_bytes() {
        Some(ram) => (ram / 4).clamp(MIN_BUDGET_BYTES as u64, MAX_BUDGET_BYTES as u64) as usize,
        None => FALLBACK_BUDGET_BYTES,
    }
}

/// `0` is the auto sentinel; anything else is clamped to a sane window.
pub(crate) fn clamp_configured_mb(mb: usize) -> Option<usize> {
    if mb == 0 { None } else { Some(mb.clamp(CONFIGURED_MIN_MB, CONFIGURED_MAX_MB)) }
}

/// Split the machine-wide budget across the builds the compute queue may admit
/// at once, never below the old constant.
pub(crate) fn per_job_budget(total: usize, max_concurrent: usize) -> usize {
    (total / max_concurrent.max(1)).max(MIN_BUDGET_BYTES)
}

/// The budget one integration job may use, right now, on this machine.
pub fn resolve_budget_bytes(conn: &Connection, settings: &SettingsManager) -> Result<usize> {
    let configured = settings.get_integration_band_budget_mb(conn)?;
    let total = match clamp_configured_mb(configured) {
        Some(mb) => mb * 1024 * 1024,
        None => auto_budget_bytes(),
    };
    Ok(per_job_budget(total, settings.get_compute_max_concurrent(conn)?))
}

// (the `mod tests` block from Step 1 stays at the bottom)
```

Note the unused-import guard: `defaults` and `keys` are used by the settings getter added in Step 4, not here — if the compiler flags them, move the `use` into `settings/mod.rs` only and drop it from this file.

- [ ] **Step 4: Add the setting**

In `crates/athenaeum-core/src/settings/mod.rs`, next to the compute-queue entries:

```rust
// in `mod defaults`
    // Banded integration working-memory budget, MB. 0 = auto (a quarter of
    // physical RAM, clamped) — see integration::band_budget.
    pub const INTEGRATION_BAND_BUDGET_MB: &str = "0";

// in `mod keys`
    pub const INTEGRATION_BAND_BUDGET_MB: &str = "integration.band_budget_mb";
```

and the getter on `SettingsManager`:

```rust
    /// Configured banded-integration memory budget in MB. `0` is the auto
    /// sentinel and passes through untouched; any other value is clamped by
    /// `band_budget::clamp_configured_mb`. An unparseable value degrades to
    /// auto rather than failing a build — same defense-in-depth stance as
    /// `get_compute_max_concurrent`, against a value that reached the row by a
    /// direct DB edit, a settings import or a botched migration.
    pub fn get_integration_band_budget_mb(&self, conn: &Connection) -> Result<usize> {
        let value = self.get_with_precedence(
            conn,
            keys::INTEGRATION_BAND_BUDGET_MB,
            defaults::INTEGRATION_BAND_BUDGET_MB,
        )?;
        Ok(value.parse().unwrap_or(0))
    }
```

- [ ] **Step 5: Declare the module and the Windows dependency**

`crates/athenaeum-core/src/integration/mod.rs`:

```rust
pub mod band_budget;
```

`crates/athenaeum-core/Cargo.toml`, a new target section (0.59 is already in `Cargo.lock` transitively, so this costs no new build time):

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_System_SystemInformation"] }
```

- [ ] **Step 6: Point the harness at the real policy**

In `crates/athenaeum-core/examples/band_profile.rs`, replace the hardcoded `256 * 1024 * 1024` fallback with `athenaeum_core::integration::band_budget::auto_budget_bytes()`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p athenaeum-core band_budget && cargo test -p athenaeum-core --lib settings::`
Expected: PASS.

Run: `cargo check -p athenaeum-core --no-default-features`
Expected: clean — `band_budget` must not reach into anything `render`- or `solver`-gated.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/integration/band_budget.rs \
        crates/athenaeum-core/src/integration/mod.rs \
        crates/athenaeum-core/src/settings/mod.rs \
        crates/athenaeum-core/Cargo.toml Cargo.lock \
        crates/athenaeum-core/examples/band_profile.rs
git commit -m "feat(integration): resolve the band budget from RAM, container limits and compute concurrency"
```

---

### Task 3: Wire the resolved budget into the master build

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (`run_build`, replacing the two `BAND_BUDGET_BYTES` arguments from Task 1)
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (delete `BAND_BUDGET_BYTES`)
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs`, `crates/athenaeum-core/src/calibration_library/cosmetic.rs` (import the floor from its new home)

**Interfaces:**
- Consumes: `band_budget::resolve_budget_bytes`, `band_budget::MIN_BUDGET_BYTES`.
- Produces: nothing new; the engine's budget argument is now fed a resolved value.

- [ ] **Step 1: Write the failing test**

In `crates/athenaeum-core/src/integration/band_budget.rs`'s test module:

```rust
    /// The build must read the operator's setting, not a constant. Pinned at
    /// the resolver because the build itself needs a real 5 GB frame set to
    /// exercise end to end.
    #[test]
    fn resolver_honours_an_explicit_setting_over_auto() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let settings = SettingsManager::new();

        let auto = resolve_budget_bytes(&conn, &settings).unwrap();
        assert_eq!(auto, per_job_budget(auto_budget_bytes(), 1), "default 0 means auto");

        settings
            .persist_setting(&conn, keys::INTEGRATION_BAND_BUDGET_MB, "512")
            .unwrap();
        assert_eq!(resolve_budget_bytes(&conn, &settings).unwrap(), 512 * 1024 * 1024);

        settings
            .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, "2")
            .unwrap();
        assert_eq!(
            resolve_budget_bytes(&conn, &settings).unwrap(),
            MIN_BUDGET_BYTES,
            "512 MB split across 2 admitted jobs is 256 MB — the floor, not below it"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core resolver_honours_an_explicit_setting_over_auto`
Expected: FAIL — `keys` and `Connection` are not in scope in the test module, or the assertion on the default path fails.

Fix the imports (`use crate::settings::keys;`, `use rusqlite::Connection;`) until the failure is the assertion, not the compile.

- [ ] **Step 3: Resolve the budget in the build**

In `crates/athenaeum-core/src/api/masters.rs`, inside `fn run_build` (`masters.rs:851` — **not** `run_master_build_thread`, which is its caller), **before** the `drop(conn)` at `masters.rs:922` that releases the pooled connection (the resolver needs it):

```rust
    // Working-memory budget for the banded reader. Resolved per build from the
    // machine and the compute-queue ceiling rather than a compile-time
    // constant — the constant was measured costing 5.3x on a 100-frame set.
    let band_budget = crate::integration::band_budget::resolve_budget_bytes(&conn, &ctx.settings)?;
```

Then pass `band_budget` instead of `crate::integration::engine::BAND_BUDGET_BYTES` at both engine call sites.

A plain `?` is correct here: `run_build` returns `Result<(i64, Option<String>), BuildStepError>` and `impl From<anyhow::Error> for BuildStepError` already exists (`masters.rs:761`), so a failed probe surfaces as a build error rather than being swallowed.

- [ ] **Step 4: Remove the constant**

Delete `pub(crate) const BAND_BUDGET_BYTES` from `engine.rs`. Its three remaining consumers move to the floor constant, which is the same 256 MiB and therefore not a behaviour change:

- `light_cal.rs:245`, `:262`, `:701` → `crate::integration::band_budget::MIN_BUDGET_BYTES`
- `cosmetic.rs:338` → same
- `engine.rs`'s own tests → same

Their doc comments should say *why* they keep the floor: these paths integrate 1–3 frames, which already yields 1–2 bands, so the budget is not their bottleneck (spec §8).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core --lib` and `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Measure — this is the task's real gate**

Evict the cache, then:

```bash
BIAS="/Volumes/bigbase2/Astrobase/Calibration/ASI2600MC DUO/ASI2600MC DUO/2024/2024-09-21/BIAS"
cargo run --release -p athenaeum-core --example band_profile -- "$BIAS" "" 0
```

Expected on the 16 GB profiling machine: auto resolves to 4 GiB, **3 bands, total ≤ 60 s** (from 241.6 s), read ≥ 100 MB/s. The `checksum` line must equal the one recorded in Task 1.

If the checksum moved, stop: the budget must not change any pixel.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/api/masters.rs \
        crates/athenaeum-core/src/integration/engine.rs \
        crates/athenaeum-core/src/integration/band_budget.rs \
        crates/athenaeum-core/src/calibration_library/light_cal.rs \
        crates/athenaeum-core/src/calibration_library/cosmetic.rs
git commit -m "perf(integration): size the band from the machine, not a constant"
```

---

### Task 4: Band planes hold source bytes

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (`BandPlanes`, `PlaneKind`, `read_band`, `band_rows_for_budget` as a method; delete the free function)
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (`run_banded`, `integrate_flat_inner`)
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (band loop, `read_full_flat_plane`, and the `band_rows_for_budget` assertion at `:1686`)
- Modify: `crates/athenaeum-core/src/calibration_library/cosmetic.rs` (`read_full_plane`)

**Interfaces:**
- Consumes: `BandSource` from Task 1/3 unchanged in construction.
- Produces:
  - `BandPlanes::new(src: &BandSource) -> BandPlanes`
  - `BandPlanes::frame_count(&self) -> usize`, `BandPlanes::rows(&self) -> usize`
  - `BandPlanes::sample(&self, frame: usize, idx: usize) -> f32`
  - `BandPlanes::decode_row_into(&self, row_in_band: usize, dst: &mut [f32])` — `dst.len() == frame_count * width`, frame-major
  - `BandPlanes::decode_frame_into(&self, frame: usize, dst: &mut [f32])` — `dst.len() == rows * width`
  - `BandSource::band_rows_for_budget(&self, budget_bytes: usize) -> usize`
  - `BandSource::read_band(&mut self, y0: usize, rows: usize, out: &mut BandPlanes) -> Result<(), IntegrationError>` — Task 6 turns this into `&self` and adds a `concurrency` argument; keep the body free of anything that would block that (no cursor, no `&mut` state beyond `out`)

- [ ] **Step 1: Write the failing tests**

In `banded.rs`'s test module (helpers `f32_fixture` and `u16_fixture` already exist there):

```rust
    #[test]
    fn planes_decode_identically_to_the_old_f32_path() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = f32_fixture(dir.path(), "a.fits", 32, 24, |x, y| (y * 32 + x) as f32);
        let p2 = u16_fixture(dir.path(), "b.fits", 32, 24, 1000);
        let mut src = BandSource::open(&[p1, p2], dir.path()).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band(10, 4, &mut planes).unwrap();

        assert_eq!(planes.frame_count(), 2);
        assert_eq!(planes.rows(), 4);
        // frame 0: f32 gradient; row 10 col 0 is 10*32
        assert_eq!(planes.sample(0, 0), (10 * 32) as f32);
        assert_eq!(planes.sample(0, 4 * 32 - 1), (13 * 32 + 31) as f32);
        // frame 1: BITPIX 16 with BZERO 32768 — physical value, not stored
        assert_eq!(planes.sample(1, 0), 1000.0);

        // decode_row_into agrees with sample(), frame-major
        let mut row = vec![0f32; 2 * 32];
        planes.decode_row_into(2, &mut row);
        for x in 0..32 {
            assert_eq!(row[x], planes.sample(0, 2 * 32 + x), "frame 0 col {x}");
            assert_eq!(row[32 + x], planes.sample(1, 2 * 32 + x), "frame 1 col {x}");
        }

        // decode_frame_into agrees too
        let mut whole = vec![0f32; 4 * 32];
        planes.decode_frame_into(0, &mut whole);
        for i in 0..4 * 32 {
            assert_eq!(whole[i], planes.sample(0, i), "sample {i}");
        }
    }

    #[test]
    fn u16_sources_get_twice_the_rows_of_f32_sources() {
        let dir = tempfile::tempdir().unwrap();
        let u16s: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("u{i}.fits"), 100, 200, 500))
            .collect();
        let f32s: Vec<_> = (0..4)
            .map(|i| f32_fixture(dir.path(), &format!("f{i}.fits"), 100, 200, |_, _| 1.0))
            .collect();
        let budget = 1024 * 1024;
        let u_rows = BandSource::open(&u16s, dir.path()).unwrap().band_rows_for_budget(budget);
        let f_rows = BandSource::open(&f32s, dir.path()).unwrap().band_rows_for_budget(budget);
        assert!(
            u_rows > f_rows,
            "BITPIX 16 bands are half the bytes of f32 bands, so the same budget must buy more rows: {u_rows} vs {f_rows}"
        );
    }

    #[test]
    fn band_rows_floor_never_overrides_budget() {
        // 3000 frames of width 9576 (2026-08-02 audit I5): one row per band is
        // slow but bounded; the floor must yield to the budget.
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|i| u16_fixture(dir.path(), &format!("t{i}.fits"), 9576, 8, 1))
            .collect();
        let src = BandSource::open(&paths, dir.path()).unwrap();
        assert_eq!(src.band_rows_for_budget(1), 1, "budget of 1 byte still yields exactly one row");
    }
```

Delete the old `reads_f32_fits_bands_exactly`, `u16_bzero_applied` and `band_rows_budget_math` tests — the first two are subsumed by `planes_decode_identically_to_the_old_f32_path`, and the third asserted the free function's f32 arithmetic, which no longer exists. Keep `dimension_mismatch_rejected`, `concurrent_spills_at_same_idx_never_cross_contaminate` and `probe_bitpix_reports_source_depth` unchanged.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib integration::banded`
Expected: FAIL to compile — `BandPlanes` does not exist.

- [ ] **Step 3: Implement `PlaneKind` and `BandPlanes`**

In `banded.rs`, replacing the decode arm of `read_band`:

```rust
/// How one frame's raw band bytes decode to physical samples. Carried per
/// frame because a set may legally mix bit depths, and because the
/// decode-and-spill fallback produces little-endian f32 while FITS is big.
#[derive(Clone, Copy)]
pub(crate) enum PlaneKind {
    U8 { bzero: f32, bscale: f32 },
    I16Be { bzero: f32, bscale: f32 },
    I32Be { bzero: f32, bscale: f32 },
    F32Be { bzero: f32, bscale: f32 },
    F64Be { bzero: f64, bscale: f64 },
    /// Decode-and-spill scratch: little-endian f32, already physical.
    F32Le,
}

impl PlaneKind {
    #[inline]
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            PlaneKind::U8 { .. } => 1,
            PlaneKind::I16Be { .. } => 2,
            PlaneKind::I32Be { .. } | PlaneKind::F32Be { .. } | PlaneKind::F32Le => 4,
            PlaneKind::F64Be { .. } => 8,
        }
    }

    #[inline]
    fn decode(self, b: &[u8], idx: usize) -> f32 {
        match self {
            PlaneKind::U8 { bzero, bscale } => b[idx] as f32 * bscale + bzero,
            PlaneKind::I16Be { bzero, bscale } => {
                let o = idx * 2;
                i16::from_be_bytes([b[o], b[o + 1]]) as f32 * bscale + bzero
            }
            PlaneKind::I32Be { bzero, bscale } => {
                let o = idx * 4;
                i32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f32 * bscale + bzero
            }
            PlaneKind::F32Be { bzero, bscale } => {
                let o = idx * 4;
                f32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) * bscale + bzero
            }
            PlaneKind::F64Be { bzero, bscale } => {
                let o = idx * 8;
                let v = f64::from_be_bytes([
                    b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
                ]);
                (v * bscale + bzero) as f32
            }
            PlaneKind::F32Le => {
                let o = idx * 4;
                f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
            }
        }
    }

    /// Bulk decode of `dst.len()` consecutive samples starting at `start` —
    /// a tight typed loop per arm so the optimizer can vectorize it.
    fn decode_run(self, b: &[u8], start: usize, dst: &mut [f32]) {
        let bpp = self.bytes_per_sample();
        let src = &b[start * bpp..(start + dst.len()) * bpp];
        match self {
            PlaneKind::U8 { bzero, bscale } => {
                for (s, d) in src.iter().zip(dst.iter_mut()) { *d = *s as f32 * bscale + bzero; }
            }
            PlaneKind::I16Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(2).zip(dst.iter_mut()) {
                    *d = i16::from_be_bytes([c[0], c[1]]) as f32 * bscale + bzero;
                }
            }
            PlaneKind::I32Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f32 * bscale + bzero;
                }
            }
            PlaneKind::F32Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = f32::from_be_bytes([c[0], c[1], c[2], c[3]]) * bscale + bzero;
                }
            }
            PlaneKind::F64Be { bzero, bscale } => {
                for (c, d) in src.chunks_exact(8).zip(dst.iter_mut()) {
                    let v = f64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                    *d = (v * bscale + bzero) as f32;
                }
            }
            PlaneKind::F32Le => {
                for (c, d) in src.chunks_exact(4).zip(dst.iter_mut()) {
                    *d = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                }
            }
        }
    }
}

/// One band of every frame, held in the SOURCE's own sample format.
///
/// Before 2026-09-06 the band was `Vec<Vec<f32>>`, so a BITPIX 16 camera file
/// was widened on the way in and every band cost twice what the data does.
/// Holding raw bytes halves band memory for the common case, which buys twice
/// the rows for the same budget — i.e. half the seek rounds. The widening
/// happens per sample inside the parallel combine, where it is nearly free.
pub struct BandPlanes {
    bufs: Vec<Vec<u8>>,
    kinds: Vec<PlaneKind>,
    width: usize,
    rows: usize,
}

impl BandPlanes {
    pub fn new(src: &BandSource) -> BandPlanes {
        BandPlanes {
            bufs: vec![Vec::new(); src.frame_count()],
            kinds: src.plane_kinds(),
            width: src.width(),
            rows: 0,
        }
    }

    pub fn frame_count(&self) -> usize { self.kinds.len() }
    pub fn rows(&self) -> usize { self.rows }

    /// One decoded sample. `idx` is `row_in_band * width + x`.
    #[inline]
    pub fn sample(&self, frame: usize, idx: usize) -> f32 {
        self.kinds[frame].decode(&self.bufs[frame], idx)
    }

    /// Every frame's samples for one row of the band, frame-major:
    /// `dst[frame * width + x]`. `dst.len()` must be `frame_count * width`.
    pub fn decode_row_into(&self, row_in_band: usize, dst: &mut [f32]) {
        let w = self.width;
        assert_eq!(dst.len(), self.frame_count() * w, "decode_row_into: dst must be frame_count * width");
        for (i, kind) in self.kinds.iter().enumerate() {
            kind.decode_run(&self.bufs[i], row_in_band * w, &mut dst[i * w..(i + 1) * w]);
        }
    }

    /// One frame's whole band. `dst.len()` must be `rows * width`.
    pub fn decode_frame_into(&self, frame: usize, dst: &mut [f32]) {
        assert_eq!(dst.len(), self.rows * self.width, "decode_frame_into: dst must be rows * width");
        self.kinds[frame].decode_run(&self.bufs[frame], 0, dst);
    }
}
```

`FrameReader` gains a `kind: PlaneKind` field built in `BandSource::open` from the probe (`Fits` maps BITPIX 8/16/32/-32/-64 with its BZERO/BSCALE; `Scratch` is always `F32Le`), and `BandSource` gains:

```rust
    pub(crate) fn plane_kinds(&self) -> Vec<PlaneKind> {
        self.readers.iter().map(|r| r.kind).collect()
    }

    /// Rows per band whose band buffers fit `budget_bytes`. Counts every
    /// frame's OWN bytes-per-row — so a BITPIX 16 set gets twice the rows an
    /// f32 set does — plus two f32 rows of headroom, the margin the old
    /// `frame_count + 2` formula carried as two phantom frames.
    ///
    /// Floor of 1: the floor must never override the budget (2026-08-02 audit
    /// I5) — at very large frame counts a 16-row floor grew band memory
    /// unbounded. One row per band is slow but bounded.
    pub fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
        let per_row: usize = self
            .readers
            .iter()
            .map(|r| self.width.saturating_mul(r.kind.bytes_per_sample()))
            .sum::<usize>()
            .saturating_add(self.width.saturating_mul(8))
            .max(1);
        (budget_bytes / per_row).max(1)
    }
```

`read_band`'s signature becomes `(&mut self, y0: usize, rows: usize, out: &mut BandPlanes)`; per frame it resizes `out.bufs[i]` to `rows * width * bpp` and reads the bytes straight in — **no decode in this function any more**. Set `out.rows = rows` before returning. Delete the free `band_rows_for_budget` function.

- [ ] **Step 4: Migrate the four consumers**

`engine.rs::run_banded` — `band_bufs` becomes `let mut planes = BandPlanes::new(&src);`, `band_rows` comes from `src.band_rows_for_budget(band_budget_bytes).min(h)`, and the gather changes one line:

```rust
                        for i in 0..n {
                            let mut v = planes.sample(i, idx);
```

Everything after that line — precal, scale, the finiteness check, `column.push`, `combine_pixel` — is untouched.

`engine.rs::integrate_flat_inner` pass 1 — same substitution, `frame[r * w + x]` becomes `planes.sample(i, r * w + x)` with the frame loop over `0..n`.

`light_cal.rs` band loop — `band_bufs` becomes a `BandPlanes`; every `band_bufs[i][idx]` becomes `planes.sample(i, idx)`.

`light_cal.rs::read_full_flat_plane` and `cosmetic.rs::read_full_plane` — replace `data[..].copy_from_slice(&bufs[0])` with `planes.decode_frame_into(0, &mut data[y * w..(y + rows) * w])`.

`light_cal.rs:1686` — `assert_eq!(band_rows_for_budget(w, 2, 300), 3, ..)` becomes an assertion on the opened source's method; if the fixture's byte arithmetic no longer yields 3, adjust the budget in the assertion so the fixture still produces a multi-band run, and say so in the message.

While in `light_cal.rs`, fix the doc comment at `:714` that names another codebase (repo rule; spec §8).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core --lib integration:: && cargo test -p athenaeum-core --lib calibration_library:: && cargo test --workspace`
Expected: PASS. `multi_band_precal_uses_global_row_index` and `non_finite_samples_are_excluded_not_propagated` are the two that would catch a botched gather.

- [ ] **Step 6: Measure — the gate**

Evict, then run the harness at auto budget.
Expected: **≤ 45 s**, bands halved relative to Task 3 (3 → 2 on the profiling machine), read ≥ 130 MB/s. **The `checksum` line must still equal Task 1's** — this task moves where decoding happens, never what it produces.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/integration/banded.rs \
        crates/athenaeum-core/src/integration/engine.rs \
        crates/athenaeum-core/src/calibration_library/light_cal.rs \
        crates/athenaeum-core/src/calibration_library/cosmetic.rs
git commit -m "perf(integration): band buffers hold source bytes, not widened f32"
```

---

### Task 5: Storage class and the I/O policy

The frames a build reads may sit on a local disk, an SSD, or a **network mount** — the owner's calibration lives on external SATA today and on a NAS tomorrow. Network storage does not merely prefer a different number, it inverts the architecture: it is latency-bound, so it wants *more* outstanding reads than the machine has cores, and a rayon pool cannot deliver that (a pool caps parallelism at its own thread count). That is why this task lands before the parallel-read task and not after a future measurement.

**Files:**
- Create: `crates/athenaeum-core/src/integration/storage_class.rs`
- Create: `crates/athenaeum-core/src/integration/io_policy.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs`
- Modify: `crates/athenaeum-core/src/settings/mod.rs`
- Modify: `crates/athenaeum-core/Cargo.toml` (one more `windows-sys` feature)
- Modify: `crates/athenaeum-core/src/integration/engine.rs`, `crates/athenaeum-core/src/api/masters.rs` (swap the loose `band_budget_bytes: usize` for `io: IoPolicy`)
- Modify: `crates/athenaeum-core/examples/band_profile.rs` (report the class, allow an override)

**Interfaces:**
- Consumes: `band_budget::resolve_budget_bytes` (Task 2).
- Produces:
  - `storage_class::StorageClass { Local, Network }` (`Copy`, `Debug`, `PartialEq`)
  - `storage_class::classify(path: &Path) -> StorageClass`
  - `storage_class::classify_all(paths: &[PathBuf]) -> StorageClass`
  - `storage_class::read_concurrency(class: StorageClass, configured: usize, pool_threads: usize) -> usize`
  - `storage_class::is_network_magic(f_type: i64) -> bool` (`pub(crate)`, Linux table, testable off-platform)
  - `storage_class::is_unc(path: &Path) -> bool` (`pub(crate)`, testable off-platform)
  - `io_policy::IoPolicy { band_budget_bytes: usize, read_concurrency: usize, storage: StorageClass }`
  - `io_policy::resolve(conn: &Connection, settings: &SettingsManager, paths: &[PathBuf], pool_threads: usize) -> Result<IoPolicy>`
  - `settings::keys::INTEGRATION_READ_CONCURRENCY = "integration.read_concurrency"`, `defaults::INTEGRATION_READ_CONCURRENCY = "0"`, `SettingsManager::get_integration_read_concurrency`

- [ ] **Step 1: Write the failing tests**

`crates/athenaeum-core/src/integration/storage_class.rs`, test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_needs_more_readers_than_cores_and_local_does_not() {
        // Local: exactly the CPU pool. The profiled 7200 rpm drive got FASTER
        // with 10-way concurrency (research §3.1), so there is no measured
        // case for giving a spinning disk fewer readers than an SSD.
        assert_eq!(read_concurrency(StorageClass::Local, 0, 10), 10);
        assert_eq!(read_concurrency(StorageClass::Local, 0, 4), 4);

        // Network: latency-bound. A 4-core box must still be able to fill a
        // LAN mount, and a big box must not flood a slow uplink.
        assert_eq!(read_concurrency(StorageClass::Network, 0, 4), 8, "floor");
        assert_eq!(read_concurrency(StorageClass::Network, 0, 10), 20);
        assert_eq!(read_concurrency(StorageClass::Network, 0, 32), 32, "ceiling");
        assert!(
            read_concurrency(StorageClass::Network, 0, 4) > 4,
            "a network mount must be able to exceed the core count — this is exactly what a rayon pool cannot do"
        );

        // An explicit setting wins over both, still bounded.
        assert_eq!(read_concurrency(StorageClass::Network, 6, 10), 6);
        assert_eq!(read_concurrency(StorageClass::Local, 999, 10), READ_CONCURRENCY_MAX);
        assert_eq!(read_concurrency(StorageClass::Local, 0, 0), 1, "never zero readers");
    }

    #[test]
    fn linux_network_filesystem_magics_are_recognised() {
        assert!(is_network_magic(0x6969), "NFS");
        assert!(is_network_magic(0xFF53_4D42), "CIFS/SMB1");
        assert!(is_network_magic(0xFE53_4D42), "SMB2/SMB3");
        assert!(is_network_magic(0x517B), "old smbfs");
        assert!(is_network_magic(0x0102_1997), "9P");
        assert!(!is_network_magic(0xEF53), "ext4 is local");
        assert!(!is_network_magic(0x9123_683E), "btrfs is local");
        assert!(!is_network_magic(0x0102_1994), "tmpfs is local");
    }

    #[test]
    fn unc_paths_are_network_by_construction() {
        assert!(is_unc(std::path::Path::new(r"\\nas\astro\bias")));
        assert!(is_unc(std::path::Path::new(r"\\?\UNC\nas\astro")));
        assert!(!is_unc(std::path::Path::new(r"D:\astro\bias")));
        assert!(!is_unc(std::path::Path::new(r"\\?\D:\astro")), "verbatim local drive is not UNC");
        assert!(!is_unc(std::path::Path::new("/Volumes/bigbase2/astro")));
    }

    #[test]
    fn a_temp_dir_classifies_as_local() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(classify(dir.path()), StorageClass::Local);
    }

    #[test]
    fn any_network_member_makes_the_whole_set_network() {
        // A set spanning a NAS and a local disk gets the network policy: the
        // extra readers are what the NAS files need, and the local files were
        // measured tolerating them.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fits");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(classify_all(&[p]), StorageClass::Local);
        assert_eq!(classify_all(&[]), StorageClass::Local, "empty set must not panic");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib integration::storage_class`
Expected: FAIL to compile — the module is not declared.

- [ ] **Step 3: Implement the classifier**

```rust
//! Which kind of storage a set of frames lives on, and how many reads to keep
//! in flight against it.
//!
//! **Two classes, not three.** The obvious third — rotational vs solid state —
//! is deliberately absent. The profiled 7200 rpm SATA drive got *faster* with
//! 10-way concurrency (research §3.1), so there is no measured case for giving
//! a spinning disk fewer readers than an SSD, and answering "is this rotating"
//! on macOS needs IOKit for a verdict nothing would act on. What genuinely
//! inverts the policy is a NETWORK mount: it is latency-bound rather than
//! seek-bound, so it wants MORE outstanding requests than the machine has
//! cores — which is why read concurrency cannot ride the CPU thread pool.
//!
//! Detection is a deterministic OS property on every platform, never a timing
//! probe: a probe is non-deterministic, spends its first bands on a knowingly
//! wrong setting, and would be auto-tuning the smaller of the two levers.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageClass {
    /// Anything the OS calls local: internal or attached, rotating or solid.
    Local,
    /// NFS / SMB / AFP / WebDAV / 9P / a mapped or UNC network drive.
    Network,
}

/// Hard ceiling on an explicitly configured reader count. Past this the only
/// thing that grows is the number of requests a server has to fan out.
pub const READ_CONCURRENCY_MAX: usize = 64;

/// How many reads to keep in flight. `configured` is
/// `integration.read_concurrency`; `0` means "decide from the class".
pub fn read_concurrency(class: StorageClass, configured: usize, pool_threads: usize) -> usize {
    if configured != 0 {
        return configured.clamp(1, READ_CONCURRENCY_MAX);
    }
    match class {
        StorageClass::Local => pool_threads.max(1),
        // Latency-bound: the link is filled by outstanding requests, not by
        // cores. Floor 8 so a 4-core box still fills a LAN mount; ceiling 32
        // so a slow uplink is not flooded with streams the server must serve
        // in parallel. Both bounds are reasoned, not measured — the network
        // measurement is an open item, and the setting is the escape hatch
        // until it exists.
        StorageClass::Network => (pool_threads.saturating_mul(2)).clamp(8, 32),
    }
}

/// The class of a whole frame set. Probes each DISTINCT parent directory
/// (normally one) and returns `Network` if any of them is: the extra readers
/// are what the network members need, and the local members were measured
/// tolerating them. An empty set is `Local`.
pub fn classify_all(paths: &[PathBuf]) -> StorageClass {
    let parents: BTreeSet<&Path> = paths.iter().filter_map(|p| p.parent()).collect();
    if parents.iter().any(|d| classify(d) == StorageClass::Network) {
        StorageClass::Network
    } else {
        StorageClass::Local
    }
}

/// The class of one path. Walks up to the nearest existing ancestor, the same
/// defensive shape `file_op::planner::device_id_for` uses. Any probe failure
/// is `Local` — the conservative answer, since it never exceeds the core count.
pub fn classify(path: &Path) -> StorageClass {
    let mut cur = path;
    loop {
        if cur.exists() {
            return classify_existing(cur);
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return StorageClass::Local,
        }
    }
}
```

macOS — `MNT_LOCAL` is the kernel's own answer, set for every physical or attached filesystem and clear for `nfs`/`smbfs`/`afpfs`/`webdav`:

```rust
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn classify_existing(path: &Path) -> StorageClass {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return StorageClass::Local;
    };
    // SAFETY: `buf` is a correctly sized zeroed statfs owned by this frame and
    // `c_path` is NUL-terminated; statfs only writes into buf.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return StorageClass::Local;
    }
    if buf.f_flags & (libc::MNT_LOCAL as u32) != 0 {
        StorageClass::Local
    } else {
        StorageClass::Network
    }
}
```

Linux — the filesystem magic, which is what `statfs(2)` actually reports:

```rust
/// Filesystem magics that mean "the bytes are on another machine". Anything
/// not listed is treated as local, which is the conservative direction: an
/// unrecognised filesystem gets the core-count policy, never more.
pub(crate) fn is_network_magic(f_type: i64) -> bool {
    const NETWORK_MAGICS: &[i64] = &[
        0x6969,      // NFS (and NFS4)
        0xFF53_4D42, // CIFS / SMB1
        0xFE53_4D42, // SMB2 / SMB3
        0x517B,      // old smbfs
        0x0102_1997, // 9P (v9fs)
        0x00c3_6400, // CephFS
        0x5346_414F, // AFS (OpenAFS)
        0x6B41_4653, // AFS (kAFS)
        0x0BD0_0BD0, // Lustre
    ];
    NETWORK_MAGICS.contains(&f_type)
}

#[cfg(target_os = "linux")]
fn classify_existing(path: &Path) -> StorageClass {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return StorageClass::Local;
    };
    // SAFETY: as above.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return StorageClass::Local;
    }
    if is_network_magic(buf.f_type as i64) { StorageClass::Network } else { StorageClass::Local }
}
```

Windows — a UNC path is network by construction; otherwise ask the OS about the volume root. Note the verbatim prefix has two forms and only `\\?\UNC\` is remote:

```rust
pub(crate) fn is_unc(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.starts_with(r"UNC\") || (s.starts_with(r"\\") && !s.starts_with(r"\\?\"))
}

#[cfg(windows)]
fn classify_existing(path: &Path) -> StorageClass {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, DRIVE_REMOTE};
    if is_unc(path) {
        return StorageClass::Network;
    }
    let root = match path.components().next() {
        Some(std::path::Component::Prefix(p)) => {
            let mut r = std::path::PathBuf::from(p.as_os_str());
            r.push("\\");
            r
        }
        _ => return StorageClass::Local,
    };
    let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer owned by this frame.
    if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_REMOTE {
        StorageClass::Network
    } else {
        StorageClass::Local
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn classify_existing(_path: &Path) -> StorageClass {
    StorageClass::Local
}
```

`is_unc` and `is_network_magic` are compiled on every platform (not behind `#[cfg]`) so their tests run everywhere; only the `classify_existing` arms are platform-gated.

Add the feature in `crates/athenaeum-core/Cargo.toml`, extending the section Task 2 created:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_System_SystemInformation", "Win32_Storage_FileSystem"] }
```

- [ ] **Step 4: Add the setting and the combined policy**

`settings/mod.rs`, beside the budget key from Task 2:

```rust
// in `mod defaults`
    // Reads kept in flight per integration. 0 = auto (from the storage class:
    // the CPU pool size locally, more on a network mount, which is
    // latency-bound). See integration::storage_class.
    pub const INTEGRATION_READ_CONCURRENCY: &str = "0";

// in `mod keys`
    pub const INTEGRATION_READ_CONCURRENCY: &str = "integration.read_concurrency";
```

```rust
    /// Configured reads-in-flight per integration. `0` is the auto sentinel;
    /// `storage_class::read_concurrency` applies the bounds. An unparseable
    /// value degrades to auto rather than failing a build — same stance as
    /// `get_compute_max_concurrent`.
    pub fn get_integration_read_concurrency(&self, conn: &Connection) -> Result<usize> {
        let value = self.get_with_precedence(
            conn,
            keys::INTEGRATION_READ_CONCURRENCY,
            defaults::INTEGRATION_READ_CONCURRENCY,
        )?;
        Ok(value.parse().unwrap_or(0))
    }
```

`crates/athenaeum-core/src/integration/io_policy.rs`:

```rust
//! The two I/O knobs an integration run needs, resolved together: how much
//! memory a band may use, and how many reads to keep in flight. They are one
//! value because they are decided from the same two inputs — the machine and
//! the storage the frames actually live on — and because passing two loose
//! `usize`s through the engine invites transposing them.

use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

use super::band_budget;
use super::storage_class::{self, StorageClass};
use crate::settings::SettingsManager;

#[derive(Clone, Copy, Debug)]
pub struct IoPolicy {
    pub band_budget_bytes: usize,
    pub read_concurrency: usize,
    /// Reported in the build's log line so a later measurement on a NAS or an
    /// SSD can be read against the policy that produced it.
    pub storage: StorageClass,
}

pub fn resolve(
    conn: &Connection,
    settings: &SettingsManager,
    paths: &[PathBuf],
    pool_threads: usize,
) -> Result<IoPolicy> {
    let storage = storage_class::classify_all(paths);
    Ok(IoPolicy {
        band_budget_bytes: band_budget::resolve_budget_bytes(conn, settings)?,
        read_concurrency: storage_class::read_concurrency(
            storage,
            settings.get_integration_read_concurrency(conn)?,
            pool_threads,
        ),
        storage,
    })
}
```

Declare both in `integration/mod.rs`:

```rust
pub mod io_policy;
pub mod storage_class;
```

- [ ] **Step 5: Swap the engine's loose budget for the policy**

Mechanical: `integrate_bias_like`, `integrate_flat`, their `_inner` twins and `run_banded` take `io: IoPolicy` where they took `band_budget_bytes: usize`, and the one use site becomes `src.band_rows_for_budget(io.band_budget_bytes)`. `read_concurrency` is unused until Task 6 — that is fine and expected; do **not** add a `#[allow(dead_code)]` for it, the field is read by `IoPolicy`'s `Debug`.

In `api/masters.rs::run_build`, replace the `band_budget` binding from Task 3 with:

```rust
    // Both I/O knobs, resolved from the machine AND from the storage the
    // frames actually live on — the same set may sit on a local disk today
    // and a NAS tomorrow.
    let io = crate::integration::io_policy::resolve(&conn, &ctx.settings, &paths, pool.current_num_threads())?;
```

placed after `paths` is collected and before the `drop(conn)` at `masters.rs:922`.

- [ ] **Step 6: Teach the harness about the class**

In `examples/band_profile.rs`, resolve the class from the paths, print it, and accept a 5th argument overriding the reader count:

```rust
    let storage = athenaeum_core::integration::storage_class::classify_all(&paths);
    let readers: usize = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(0);
    let io = athenaeum_core::integration::io_policy::IoPolicy {
        band_budget_bytes: budget,
        read_concurrency: athenaeum_core::integration::storage_class::read_concurrency(
            storage, readers, pool.current_num_threads(),
        ),
        storage,
    };
    println!("storage {:?}, {} readers", io.storage, io.read_concurrency);
```

and pass `io` to `integrate_bias_like` in place of `budget`.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p athenaeum-core --lib integration:: && cargo test --workspace && cargo check -p athenaeum-core --no-default-features`
Expected: PASS and clean. No timing gate for this task — it changes no I/O yet; Task 6 is where the concurrency is actually used.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/integration/storage_class.rs \
        crates/athenaeum-core/src/integration/io_policy.rs \
        crates/athenaeum-core/src/integration/mod.rs \
        crates/athenaeum-core/src/integration/engine.rs \
        crates/athenaeum-core/src/settings/mod.rs \
        crates/athenaeum-core/src/api/masters.rs \
        crates/athenaeum-core/Cargo.toml Cargo.lock \
        crates/athenaeum-core/examples/band_profile.rs
git commit -m "feat(integration): classify the storage the frames live on and derive the I/O policy from it"
```

---

### Task 6: Positional, parallel reads

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (`read_band`)
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (wrap the call in `pool.install`)

**Interfaces:**
- Consumes: `IoPolicy::read_concurrency` (Task 5).
- Produces:
  - `BandSource::read_band(&self, y0: usize, rows: usize, out: &mut BandPlanes, concurrency: usize) -> Result<(), IntegrationError>` — `&self`, positional, and its parallelism is an argument rather than an ambient pool.
  - `BandSource::open(paths: &[PathBuf], scratch_dir: &Path, concurrency: usize) -> Result<BandSource, IntegrationError>` — the header probe is one `open` + a few small reads **per file**, i.e. 100 round trips on a network mount; it gets the same concurrency.

- [ ] **Step 1: Write the failing test**

In `banded.rs`'s test module:

```rust
    /// Reads must be positional: `pread` per frame, no shared cursor, so the
    /// frames can be filled in parallel. Pinned by reading the SAME source
    /// twice from a shared reference — which does not compile against a
    /// `&mut self` reader, and would interleave cursors against a seeking one.
    #[test]
    fn bands_are_read_positionally_from_a_shared_source() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..8)
            .map(|i| f32_fixture(dir.path(), &format!("p{i}.fits"), 64, 40, move |x, y| (i * 10_000 + y * 64 + x) as f32))
            .collect();
        let src = BandSource::open(&paths, dir.path()).unwrap();

        let read = |y0: usize, rows: usize| {
            let mut planes = BandPlanes::new(&src);
            src.read_band(y0, rows, &mut planes).unwrap();
            (0..planes.frame_count())
                .map(|f| (0..rows * 64).map(|i| planes.sample(f, i)).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };

        let a = read(8, 4);
        let b = read(8, 4);
        assert_eq!(a, b, "the same band must read identically — a shared cursor would drift");
        for (f, plane) in a.iter().enumerate() {
            assert_eq!(plane[0], (f * 10_000 + 8 * 64) as f32, "frame {f} row 8 col 0");
        }

        // Two bands read concurrently off the same &BandSource.
        let (c, d) = rayon::join(|| read(0, 4), || read(20, 4));
        assert_eq!(c[3][0], (3 * 10_000) as f32);
        assert_eq!(d[3][0], (3 * 10_000 + 20 * 64) as f32);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib bands_are_read_positionally_from_a_shared_source`
Expected: FAIL to compile — `read_band` takes `&mut self`.

- [ ] **Step 3: Make the reads positional and parallel**

At the top of `banded.rs`:

```rust
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
```

Give `FrameReader` a positional fill that never touches a cursor:

```rust
impl FrameReader {
    /// Fill `buf` from `offset`. Positional — no seek, no cursor — so several
    /// bands (and several frames of one band) can be read from a shared
    /// `&BandSource` at once.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let (file, base) = match self {
            FrameReader::Fits { file, data_offset, .. } => (file, *data_offset),
            FrameReader::Scratch { file } => (file, 0),
        };
        #[cfg(unix)]
        {
            file.read_exact_at(buf, base + offset)
        }
        #[cfg(windows)]
        {
            // `seek_read` may return a short read; loop until the buffer is
            // full or the file ends. There is no `read_exact_at` on Windows.
            let mut done = 0usize;
            while done < buf.len() {
                let n = file.seek_read(&mut buf[done..], base + offset + done as u64)?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "short read while filling a band",
                    ));
                }
                done += n;
            }
            Ok(())
        }
    }
}
```

`read_band` becomes `&self`, takes its parallelism as an argument, and runs it
on scoped OS threads — **not** on the rayon pool:

```rust
    /// Reads rows [y0, y0+rows) of every frame into `out`, in the source's own
    /// sample format, BZERO/BSCALE metadata carried but not applied, CFA
    /// untouched.
    ///
    /// `concurrency` comes from `IoPolicy` (Task 5) and is deliberately NOT the
    /// rayon pool's width. A rayon pool caps parallelism at its own thread
    /// count, which is derived from CPU cores — and a network mount is
    /// latency-bound, so it needs MORE outstanding reads than the machine has
    /// cores to fill the link at all. Scoped OS threads give an exact,
    /// pool-independent count, cost ~10-20 us to spawn against a band read
    /// measured in seconds, and are safe to use from inside a
    /// `pool.install(..)` (they are not rayon workers, so they cannot deadlock
    /// against the pool that called in).
    pub fn read_band(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
    ) -> Result<(), IntegrationError> {
        assert_eq!(out.frame_count(), self.readers.len());
        let w = self.width;
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!(
                "band {y0}+{rows} beyond height {}", self.height
            )));
        }
        out.rows = rows;

        let n = self.readers.len();
        let workers = concurrency.max(1).min(n.max(1));
        let per = n.div_ceil(workers.max(1));
        let mut errors: Vec<IntegrationError> = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for (readers, bufs) in self.readers.chunks(per).zip(out.bufs.chunks_mut(per)) {
                handles.push(scope.spawn(move || -> Result<(), IntegrationError> {
                    for (reader, buf) in readers.iter().zip(bufs.iter_mut()) {
                        let bpp = reader.kind.bytes_per_sample();
                        buf.resize(rows * w * bpp, 0);
                        reader.read_exact_at(buf, (y0 * w * bpp) as u64)?;
                    }
                    Ok(())
                }));
            }
            for h in handles {
                match h.join() {
                    Ok(Err(e)) => errors.push(e),
                    // A panicked reader must surface as an error, never as a
                    // silently short band.
                    Err(_) => errors.push(IntegrationError::BadInput(
                        "a band reader thread panicked".into(),
                    )),
                    Ok(Ok(())) => {}
                }
            }
        });
        if let Some(e) = errors.into_iter().next() { return Err(e); }
        Ok(())
    }
```

`BandSource::open` gets the same treatment: its per-file `probe_fits` is an
`open` plus a couple of 2880-byte reads, so a 100-frame set on a NAS pays 100
serial round trips before a single pixel is read (it measured 0.55 s even
locally). Probe the paths across `concurrency` scoped threads and assemble
`readers` in path order afterwards, so the frame order the caller passed — the
order `bad_samples_per_frame` is indexed by — is preserved exactly.

`BandPlanes::bufs` and `rows` must be reachable from `BandSource` — keep both types in this module and the fields `pub(super)` or private-with-accessors; do not widen them beyond the module.

- [ ] **Step 4: Pass the concurrency at the call sites**

`engine.rs::run_banded` and `integrate_flat_inner` — no `pool.install` around
the read any more; the combine keeps its pool, the read no longer borrows it:

```rust
        let t_read = std::time::Instant::now();
        src.read_band(y0, rows, &mut planes, io.read_concurrency)?;
        read_duration += t_read.elapsed();
```

and `BandSource::open(paths, scratch_dir, io.read_concurrency)?`.

`light_cal.rs` and `cosmetic.rs` read 1–3 frames and have no `IoPolicy` at
their call sites (spec §8 keeps them on the floor budget). Pass `1` and say why
in a comment: with three frames there is nothing to parallelize, and inventing
a policy for them would widen this cycle into the calibrated-export path.

`src` no longer needs `mut` in any of the four consumers — drop the `mut`
bindings the compiler now warns about.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p athenaeum-core --lib && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Measure**

Evict, run the harness at auto budget.
Expected: **≤ 40 s** and read **≥ 150 MB/s** — the spec's headline gate. On this
7200 rpm drive the parallelism itself is worth only ~8 %; it is in the plan
because queue depth 1 is the whole bottleneck on an SSD and because a network
mount cannot be filled at all without exceeding the core count. Checksum
unchanged from Task 1.

Also run the 30-frame dark set (`"$DARK" "1.00s" 0`), expecting **≤ 8 s**
against the 11.8 s baseline.

Then sweep the reader count on the same cold set to confirm the local policy is
not leaving anything on the table — the harness's 5th argument forces it:

```bash
for r in 1 4 10 20; do <evict>; cargo run --release -p athenaeum-core --example band_profile -- "$BIAS" "" 0 0 $r; done
```

Record the four numbers in the commit message. If 20 readers beats 10 on this
LOCAL drive by more than a few percent, say so — it would mean the local policy
should not be tied to the core count either, and that is a spec change, not a
constant tweak.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/integration/banded.rs \
        crates/athenaeum-core/src/integration/engine.rs \
        crates/athenaeum-core/src/calibration_library/light_cal.rs \
        crates/athenaeum-core/src/calibration_library/cosmetic.rs
git commit -m "perf(integration): read band frames positionally and in parallel"
```

---

### Task 7: The operator can see what the build is doing

This closes the gap that started the investigation: 13.5 minutes of work with no log line and a UI that discards the percentage it is handed.

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (`MasterBuildProgressEvent`, `run_master_build`)
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (field dictionary)
- Modify: `src/types/helpers.ts`
- Modify: `src/components/CalibrationHierarchyView.tsx`
- Modify: `src/components/calibration/CalibrationTableView.tsx`

**Interfaces:**
- Consumes: `IntegrationOutput::read_duration` / `combine_duration` (Task 1).
- Produces: `MasterBuildProgressEvent { set_id, stage, current, total, percent, bytes_done, bytes_total }` on both the Rust and TS side, snake_case on both (the struct carries no `rename_all`; do not add one).

- [ ] **Step 1: Write the failing test**

`masters.rs`'s test module already has `capture_events` (`masters.rs:3586`), which returns `(result, events)` where each event is `(level, message)` — its `Visit` impl records the `message` field only, so a test can pin the level and the exact message text but not the field values. There is no end-to-end master-build fixture in that module (`capture_events` has exactly one caller today, around `finalize_rebuild`), and standing one up would need real multi-frame FITS on disk.

So the two log emissions get extracted into named functions, which is what makes them testable at all:

```rust
    /// A multi-minute operation that logs nothing is why a running build was
    /// indistinguishable from a hung one (research §8). The message strings
    /// are the contract — `docs/logging/README.md` recipes and the log-mcp
    /// queries match on them.
    #[test]
    fn build_lifecycle_lines_are_emitted_at_info() {
        let (_, events) = capture_events(|| {
            log_build_started(42, "Dark", 30, "average+winsorized-3.0/3.0", 4 * 1024 * 1024 * 1024, 10);
        });
        assert!(
            events.iter().any(|(lvl, m)| lvl == "INFO" && m == "master build started"),
            "no start line; got {events:?}"
        );

        let (_, events) = capture_events(|| {
            log_build_finished(42, std::time::Duration::from_millis(44_400), 5_200_000_000, &nop_output());
        });
        assert!(
            events.iter().any(|(lvl, m)| lvl == "INFO" && m == "master build finished"),
            "no finish line; got {events:?}"
        );
    }
```

`nop_output()` is a two-line helper in the test module returning an `IntegrationOutput` with zeroed data and plausible `read_duration` / `combine_duration` / `band_rows` / `bands` / `bytes_read`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core a_master_build_logs_its_lifecycle_at_info`
Expected: FAIL — neither message is emitted.

- [ ] **Step 3: Log the lifecycle**

Add both functions to `api/masters.rs` and call them from `run_build` — the
start line right after the budget is resolved, the finish line right after the
engine returns:

```rust
/// Build-lifecycle log lines. Named functions rather than inline macros so a
/// test can pin their message text without a multi-gigabyte build fixture.
fn log_build_started(
    set_id: i64,
    imagetyp: &str,
    count: i64,
    recipe: &str,
    budget_bytes: usize,
    read_threads: usize,
) {
    tracing::info!(
        set_id,
        imagetyp,
        count,
        recipe,
        budget_mb = budget_bytes / (1024 * 1024),
        // Read concurrency is `image_pool`'s size, i.e. derived from core
        // count — a CPU number applied to an I/O problem (spec §8). Logged so
        // the pending SSD measurement can be read against it instead of
        // guessing what it ran at.
        read_threads,
        "master build started"
    );
}

fn log_build_finished(
    set_id: i64,
    duration: std::time::Duration,
    bytes_read: u64,
    out: &IntegrationOutput,
) {
    let read_s = out.read_duration.as_secs_f64();
    tracing::info!(
        set_id,
        duration_ms = duration.as_millis() as u64,
        read_ms = out.read_duration.as_millis() as u64,
        combine_ms = out.combine_duration.as_millis() as u64,
        read_mb_s = if read_s > 0.0 { (bytes_read as f64 / read_s / 1e6).round() as u64 } else { 0 },
        band_rows = out.band_rows,
        bands = out.bands,
        "master build finished"
    );
}
```

`bytes_read` is `out.bytes_read` (Task 1) — the engine's own count, not the
files' size on disk.

Message = short stable phrase, all data in snake_case fields — never interpolated into the message (logging spec). Add `band_rows`, `bands`, `budget_mb`, `read_threads`, `read_ms`, `combine_ms`, `read_mb_s` to the field dictionary in the logging design doc's "Unified event schema" section in the same commit; new field names require a spec update.

- [ ] **Step 4: Carry bytes in the progress event**

```rust
struct MasterBuildProgressEvent {
    set_id: i64,
    stage: &'static str,
    current: usize,
    total: usize,
    percent: f64,
    /// Bytes of source read so far / in total. `current`/`total` count bands,
    /// which say nothing about size once bands are machine-sized.
    bytes_done: u64,
    bytes_total: u64,
}
```

and the matching TS in `src/types/helpers.ts`:

```ts
export interface MasterBuildProgressEvent {
  set_id: number;
  stage: 'reading' | 'integrating' | 'writing' | 'registering';
  current: number;
  total: number;
  percent: number;
  bytes_done: number;
  bytes_total: number;
}
```

- [ ] **Step 5: Stop discarding the percentage**

`src/components/CalibrationHierarchyView.tsx:128-133` currently reduces the event to its phase:

```ts
  const { buildStates } = useMasterBuildContext();
  const buildStatusBySet = useMemo(() => {
    const m: Record<number, 'starting' | 'building' | 'done'> = {};
    for (const [id, s] of buildStates) m[id] = s.phase;
    return m;
  }, [buildStates]);
```

Keep the whole `BuildState` instead:

```ts
  const { buildStates } = useMasterBuildContext();
  // The event carries stage + percent; reducing it to a phase is what made a
  // running build indistinguishable from a hung one (research §8).
  const buildStateBySet = useMemo(() => {
    const m: Record<number, BuildState> = {};
    for (const [id, s] of buildStates) m[id] = s;
    return m;
  }, [buildStates]);
```

Update `CalibrationTableView`'s prop (its doc comment at `:36` already says "Live build phase per source set id") to take `BuildState` and render, for a building row, the stage and the rounded percent — design tokens only, e.g. `text-content-muted` for the stage word and `text-accent` for the number. Every other consumer of the old `'starting' | 'building' | 'done'` shape must be updated in the same pass; `CameraDetail.tsx:74-79` also reads `buildStates`.

- [ ] **Step 6: Run the checks**

Run: `cargo test -p athenaeum-core --lib api::masters && npx tsc --noEmit`
Expected: PASS and clean.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/api/masters.rs \
        docs/superpowers/specs/2026-07-03-logging-overhaul-design.md \
        src/types/helpers.ts src/components/CalibrationHierarchyView.tsx \
        src/components/calibration/CalibrationTableView.tsx src/components/CameraDetail.tsx
git commit -m "feat(masters): log the build lifecycle and show its progress in the calibration tree"
```

---

### Task 8: The budget control in Settings

Optional in the sense that the auto policy serves every machine — but the reference implementations expose exactly this lever, and an 8 GB machine running other work needs the escape hatch.

**Files:**
- Modify: `crates/athenaeum-core/src/api/compute.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/compute.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/compute.rs`
- Modify: `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `src/pages/Settings.tsx`

**Interfaces:**
- Consumes: `band_budget::{resolve_budget_bytes, auto_budget_bytes, total_ram_bytes}`, `SettingsManager::get_integration_band_budget_mb`.
- Produces:
  - `pub struct IntegrationBudgetInfo { configured_mb: usize, effective_mb: usize, auto_mb: usize, total_ram_mb: usize }` with `#[serde(rename_all = "camelCase")]` and `#[derive(ts_rs::TS)]`
  - `api::get_integration_band_budget(ctx) -> Result<IntegrationBudgetInfo, ApiError>`
  - `api::set_integration_band_budget(ctx, mb: usize) -> Result<(), ApiError>`
  - commands `get_integration_band_budget` / `set_integration_band_budget` on both backends

- [ ] **Step 1: Write the failing test**

`api/compute.rs`'s test module deliberately has **no** `ServiceContext` fixture — its own header says the handlers are thin and what gets pinned is "the settings-key default … and the bounds check", tested through the real pure functions. Follow that convention exactly: the resolver's behaviour is already pinned by Task 3's `resolver_honours_an_explicit_setting_over_auto`, so what is left here is the default and the info assembly.

```rust
    #[test]
    fn default_band_budget_is_auto() {
        assert_eq!(crate::settings::defaults::INTEGRATION_BAND_BUDGET_MB, "0");
    }

    #[test]
    fn budget_info_reports_configured_effective_and_auto() {
        // 700 MB configured, one admitted job, 16 GB machine.
        let info = budget_info_from(700, 700 * 1024 * 1024, 4096 * 1024 * 1024, 16384 * 1024 * 1024);
        assert_eq!(info.configured_mb, 700);
        assert_eq!(info.effective_mb, 700);
        assert_eq!(info.auto_mb, 4096, "auto is reported even when overridden, so the UI can name it");
        assert_eq!(info.total_ram_mb, 16384);

        // Auto, but two admitted jobs halve it.
        let info = budget_info_from(0, 2048 * 1024 * 1024, 4096 * 1024 * 1024, 16384 * 1024 * 1024);
        assert_eq!(info.configured_mb, 0, "0 is the auto sentinel and is reported as-is");
        assert_eq!(info.effective_mb, 2048);
        assert_eq!(info.auto_mb, 4096, "the UI must be able to say why effective < auto");

        // Failed RAM probe.
        assert_eq!(budget_info_from(0, 1024 * 1024 * 1024, 1024 * 1024 * 1024, 0).total_ram_mb, 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib api::compute`
Expected: FAIL — `INTEGRATION_BAND_BUDGET_MB` and `budget_info_from` do not exist.

- [ ] **Step 3: Implement the core handlers**

```rust
/// What the Settings control needs to show the operator both what they chose
/// and what the machine actually resolved.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IntegrationBudgetInfo {
    /// The stored `integration.band_budget_mb`. `0` means auto.
    pub configured_mb: usize,
    /// What one integration job gets right now, after the auto formula and the
    /// division by `compute.max_concurrent`.
    pub effective_mb: usize,
    /// What auto alone would resolve to on this machine.
    pub auto_mb: usize,
    /// Physical RAM the probe found; `0` when the platform probe failed.
    pub total_ram_mb: usize,
}

/// Pure assembly, split out so it is testable without a `ServiceContext` —
/// the convention this module's other tests already follow.
pub(crate) fn budget_info_from(
    configured_mb: usize,
    effective_bytes: usize,
    auto_bytes: usize,
    total_ram_bytes: u64,
) -> IntegrationBudgetInfo {
    const MB: usize = 1024 * 1024;
    IntegrationBudgetInfo {
        configured_mb,
        effective_mb: effective_bytes / MB,
        auto_mb: auto_bytes / MB,
        total_ram_mb: (total_ram_bytes / MB as u64) as usize,
    }
}

pub fn get_integration_band_budget(ctx: &ServiceContext) -> Result<IntegrationBudgetInfo, ApiError> {
    // reads the setting + resolver + auto + probe, then `budget_info_from`
}

/// Persist the budget. `0` restores auto; anything else is clamped to
/// 64..=16384 MB by the resolver, so a wild value degrades instead of
/// OOM-ing the next build.
pub fn set_integration_band_budget(ctx: &ServiceContext, mb: usize) -> Result<(), ApiError> { /* .. */ }
```

Both go through `ctx.settings` and the DB the same way `set_compute_max_concurrent` (`api/compute.rs:54`) does. Register `IntegrationBudgetInfo` in `crates/athenaeum-core/src/ts_export.rs` next to `crate::services::compute_queue::ComputeQueueEntry` (`ts_export.rs:155`), then regenerate:

```bash
TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
```

- [ ] **Step 4: Mirror on both backends**

`crates/athenaeum-tauri/src/commands/compute.rs`:

```rust
/// Read the resolved banded-integration memory budget.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_integration_band_budget(
    state: State<'_, AppState>,
) -> Result<api::IntegrationBudgetInfo, String> {
    api::get_integration_band_budget(&state.ctx).map_err(|e| e.to_string())
}

/// Persist the banded-integration memory budget. 0 = auto.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_integration_band_budget(state: State<'_, AppState>, mb: usize) -> Result<(), String> {
    api::set_integration_band_budget(&state.ctx, mb).map_err(|e| e.to_string())
}
```

Register both in `invoke_handler` in `crates/athenaeum-tauri/src/lib.rs` beside `commands::set_compute_max_concurrent` (`lib.rs:382`).

`crates/athenaeum-web/src/routes/compute.rs`, matching the file's existing shape:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIntegrationBandBudgetArgs { pub mb: usize }

/// GET /api/get_integration_band_budget
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_integration_band_budget(
    State(state): State<WebAppState>,
) -> Result<Json<api::IntegrationBudgetInfo>, (StatusCode, String)> {
    api::get_integration_band_budget(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_integration_band_budget
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_integration_band_budget(
    State(state): State<WebAppState>,
    Json(args): Json<SetIntegrationBandBudgetArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_integration_band_budget(&state.ctx, args.mb).map(Json).map_err(api_err)
}
```

Register both in `build_router` in `crates/athenaeum-web/src/routes/mod.rs` beside the `set_compute_max_concurrent` route (`mod.rs:204`).

- [ ] **Step 5: Add the control**

On the **Calibration** tab of `src/pages/Settings.tsx` (`activeTab === 'calibration'`, around `:557`), a number input following the Blink-threads control's shape (`Settings.tsx:1031-1043`):

- label: `Integration memory budget`
- input: number, `0` = auto, min 0, max 16384, step 64
- helper line: `Working memory one master build may use for reading frames. 0 = automatic ({autoMb} MB on this machine, from {totalRamMb} MB of RAM). Larger values read the disk in fewer, longer sweeps; smaller values use less memory and take longer.`
- when `effectiveMb !== configuredMb && configuredMb !== 0`, show that the value was clamped, and when `effectiveMb < autoMb` because `compute.max_concurrent > 1`, say so.

Load with `api.invoke<IntegrationBudgetInfo>('get_integration_band_budget')` on mount and save with `api.invoke('set_integration_band_budget', { mb })` in the existing save handler. Design tokens only.

- [ ] **Step 6: Run every gate**

```bash
cargo build --workspace
cargo test --workspace
cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit
```

Expected: all clean.

- [ ] **Step 7: Final end-to-end measurement**

Rebuild the desktop app, delete the 11 masters built on 2026-09-06 from `/Volumes/bigbase3/Calibration`, un-supersede their raw sets (the Delete master action does this), and re-run the LDN 1272 batch.
Expected: **≤ 5 min** against the 13.5 min baseline, with a `master build started` / `master build finished` pair per set in the log and a live percentage on each building row.

- [ ] **Step 8: Commit**

```bash
git add crates/athenaeum-core/src/api/compute.rs crates/athenaeum-core/src/ts_export.rs \
        src/types/models.ts \
        crates/athenaeum-tauri/src/commands/compute.rs crates/athenaeum-tauri/src/lib.rs \
        crates/athenaeum-web/src/routes/compute.rs crates/athenaeum-web/src/routes/mod.rs \
        src/pages/Settings.tsx
git commit -m "feat(settings): expose the integration memory budget on both backends"
```

---

## After the plan

- Add to `docs/superpowers/open-items.md`: the owner smoke of the re-run LDN 1272 batch on real data, and a measurement of the same batch on an SSD-backed scan root (where Task 5's parallelism is expected to matter far more than the 8 % it bought on the 7200 rpm drive).
- Release-note lines owed: master creation is several times faster on large calibration sets; the memory it uses now scales with the machine and is configurable; master builds report progress and appear in the log.
- Follow-ups recorded in spec §8, not in this plan: `light_cal.rs`'s serial per-pixel loop, the row-scratch decode optimization, and a subject id on `ComputeQueueEntry` so the sidebar can show a per-set percentage.
