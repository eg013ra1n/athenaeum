# Review fixes: integration throughput + Blink VNG — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the six findings the 2026-09-06 review of the integration-throughput and Blink full-resolution VNG cycles rated worth fixing: a master build's progress percent that goes backwards, a Cancel that waits for a whole band, a "finished" log line on a cancelled build, a VNG gate that starves every other image request, an unclamped cache-limit setting, and a redundant file open per frame.

**Architecture:** Four independent tasks, each confined to one seam. (1) `api::masters` derives ONE monotonic percent from the two counters the engine already reports and drops the duplicate end-of-band emission. (2) `integration::banded` threads the caller's cancel flag into its probe and read workers and reuses the probe's file handle. (3) The VNG gate becomes an async mutex the HOSTS take before the image semaphore, with a header-only CFA probe in core deciding whether to take it. (4) A single settings resolver clamps `blink.memory_cache_max_mb` at all four apply sites. No wire types, no schema, no new commands.

**Tech Stack:** Rust (athenaeum-core / athenaeum-tauri / athenaeum-web), tokio `sync::Mutex`, std atomics, existing `fits_parser` header readers. Frontend touched only for one doc comment.

**Spec:** the findings live in this plan's "Findings" section below (from the review conversation of 2026-09-06). Design context: `docs/superpowers/specs/2026-09-06-integration-throughput-design.md` (§7 D6 progress, §9 acceptance) and `docs/superpowers/specs/2026-09-06-blink-full-resolution-vng-design.md` (§4.2 the gate, §4.3 the cache budget).

## Global Constraints

- Two backends in sync: any change to a Tauri command's behaviour has its Axum mirror in the same task (`crates/athenaeum-tauri/src/commands*/…` ↔ `crates/athenaeum-web/src/routes/…`).
- Real logic in `athenaeum-core`; hosts stay thin.
- Never swallow an error: log before degrading.
- Logging: message = short stable phrase, data in snake_case fields; canonical field names only.
- Never name another codebase in code or comments (no "PixInsight", "ASTAP", …).
- Pixel maths untouched: nothing in `combine_pixel`, `BandPlanes::decode*`, the flat two-pass order, or any header card changes. `cargo test --workspace` must stay green; the pixel fingerprint `a4f6bb5158714175` pins the `I16Be` path.
- Progress payload shape (`MasterBuildProgressEvent`, snake_case, no `rename_all`) is unchanged: `set_id, stage, current, total, percent, bytes_done, bytes_total`.
- Commit as the repository owner (`eg013ra1n` / their email is already the configured git user). No AI co-author trailer — owner rule.
- Branch: `fix/review-wave-2026-09-06` cut from `main` in the main checkout (no worktree: a second `target/` on this machine's 92 %-full disk is not affordable). Merge ff into `main` at the end.
- Gates before merge: `cargo build --workspace`, `cargo test --workspace`, `cargo check -p athenaeum-core --no-default-features`, `npx tsc --noEmit`.

## Findings being fixed (from the review)

| # | Where | What | Why it matters |
| ---- | ---- | ---- | ---- |
| F1 | `api/masters.rs` `on_band`/`on_combine` | `integrating` percent = bytes fraction, `combining` percent = global rows fraction, and `run_banded` re-emits `on_band` after every band's combine | The one cell that shows a build's progress goes 50 → 0…50 → 50…100 → 50…100 on a two-band build and flips "combining 100 %" back to "integrating 100 %" before "writing" |
| F2 | `integration/banded.rs` `open` / `read_band_with_progress` | No cancel check inside the probe or read workers; errors surface only after every worker joins | A 4 GiB band at 175 MB/s means Cancel takes ~25 s to bite; seen live 2026-09-06 (cancel 19:04:07, build stopped 19:04:13) |
| F3 | `api/masters.rs` `run_build` | `log_build_finished` fires before the post-engine cancel re-check | A cancel in that window logs "master build finished" then "master build cancelled" for one `set_id` |
| F4 | `rustafits_processor/mod.rs` + both hosts | `VNG_GATE` is taken inside `process_fits_to_jpeg`, i.e. after the host's `image_semaphore` permit | A full-resolution OSC prefetch parks N−1 permits on the gate and every thumbnail/preview request on the shared semaphore waits behind the whole VNG backlog |
| F5 | `settings` + 4 apply sites | `blink.memory_cache_max_mb` has no backend clamp; literal `512` in four places; `defaults::BLINK_MEMORY_CACHE_MAX_MB` unused | A `0` from a script or hand edit silently turns the cache into one entry; the UI's 64–16384 check is the only guard |
| F6 | `integration/banded.rs` `probe_one` | `File::open` a second time after `probe_fits` already opened the file for its header | One redundant open per frame, exactly the per-file latency the parallel probe exists to amortize on a network mount |

Deliberately NOT in this plan (recorded in the review, none blocking): `Arc<[u8]>` for the cache payload, memoizing the calibration tables per tick, de-duplicating `read_full_plane`/`read_full_flat_plane`, and the sidebar percent (backlog v0.5.6 item 5).

---

### Task 1: One monotonic percent per build, and the finished line after the cancel check (F1, F3)

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (helpers next to `progress_tick_is_due` ~line 881; `run_build` closures ~lines 1041–1134; the post-engine block ~lines 1161–1179)
- Modify: `src/types/helpers.ts` (doc comment on `MasterBuildProgressEvent.percent`, ~line 384)
- Test: `crates/athenaeum-core/src/api/masters.rs` `mod tests` (bottom of file)

**Interfaces:**
- Consumes: `EngineProgress { on_band, on_combine }` from `integration::engine` — unchanged. `on_band(band_1based, bands_total, bytes_read_so_far, bytes_total)` fires once per frame read plus once at the end of every band (after that band's combine, with the same bytes as its last per-frame tick). `on_combine(rows_combined_global, rows_total, bytes_done, bytes_total)` fires every 64 rows and at the last row.
- Produces: `fn build_percent(bytes_done: u64, bytes_total: u64, rows_done: usize, rows_total: usize) -> f64` and `const READ_SHARE: f64 = 0.7`, private to `api::masters`. Event shape unchanged.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/athenaeum-core/src/api/masters.rs`:

```rust
    /// Review 2026-09-06 F1: the calibration row showed a bytes percent for
    /// `integrating` and a rows percent for `combining` in the same cell, so
    /// the number went backwards at every band boundary. One percent, built
    /// from BOTH running maxima, cannot.
    #[test]
    fn build_percent_is_monotonic_across_a_two_band_build() {
        // Two bands: read half → combine half → read the rest → combine the rest.
        let (bt, rt) = (1000u64, 100usize);
        let seq: [(u64, usize); 8] = [
            (0, 0), (250, 0), (500, 0),      // band 1 read
            (500, 25), (500, 50),            // band 1 combine
            (750, 50), (1000, 50),           // band 2 read
            (1000, 100),                     // band 2 combine
        ];
        let mut last = -1.0f64;
        for (b, r) in seq {
            let p = build_percent(b, bt, r, rt);
            assert!(p >= last, "percent went backwards: {last} -> {p} at bytes={b} rows={r}");
            last = p;
        }
        assert!((last - 100.0).abs() < 1e-9, "a finished build reads exactly 100, got {last}");
        // Reading carries READ_SHARE of the bar: fully read, nothing combined.
        let read_only = build_percent(bt, bt, 0, rt);
        assert!((read_only - READ_SHARE * 100.0).abs() < 1e-9, "got {read_only}");
    }

    #[test]
    fn build_percent_tolerates_unknown_totals_and_overshoot() {
        assert_eq!(build_percent(0, 0, 0, 0), 0.0, "nothing known yet is 0, not NaN");
        // rows_total is unknown (0) until the first combine tick: only the read share counts.
        assert!((build_percent(500, 1000, 0, 0) - 35.0).abs() < 1e-9);
        // A tick that overshoots its total (a flat's two-pass wrapper can) is capped at 100.
        assert!((build_percent(2000, 1000, 200, 100) - 100.0).abs() < 1e-9);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib api::masters::tests::build_percent -- --nocapture`
Expected: compile error, `build_percent` / `READ_SHARE` not found.

- [ ] **Step 3: Add the helper next to `progress_tick_is_due`**

Insert directly above `fn progress_tick_is_due(` in `crates/athenaeum-core/src/api/masters.rs`:

```rust
/// Share of a build's unified progress that reading accounts for. From the
/// 2026-09-06 in-app run: read 29.5 s / combine 12.7 s and 22.9 s / 14.0 s on
/// two 100-frame bias sets, 6.7 s / 3.2 s on a 30-frame dark — reading is
/// 65-70 % of the pixel phase on that hardware, so 0.7 keeps the bar moving
/// at roughly wall-clock speed. A page-cache-hot read (0.3 s / 3.4 s on the
/// dark that re-ran after a cancel) jumps the bar to 70 % early and then
/// crawls, which is honest about the work left if not about the time left.
const READ_SHARE: f64 = 0.7;

/// One 0..=100 for the whole pixel phase, from the two counters the engine
/// reports: bytes read (climbs during `integrating`) and rows combined
/// (climbs during `combining`). Review 2026-09-06 F1: the previous shape — a
/// bytes percent for one stage and a rows percent for the other, rendered in
/// the same UI cell — went backwards at every band boundary. The caller feeds
/// this RUNNING MAXIMA of both counters, so the output is monotonic by
/// construction. Each fraction is capped at 1 (a flat's two-pass wrapper can
/// report a byte pair past its own total for one tick) and an unknown total
/// (0, before the first tick of that phase) contributes nothing.
fn build_percent(bytes_done: u64, bytes_total: u64, rows_done: usize, rows_total: usize) -> f64 {
    let read = if bytes_total > 0 { (bytes_done as f64 / bytes_total as f64).min(1.0) } else { 0.0 };
    let combine = if rows_total > 0 { (rows_done as f64 / rows_total as f64).min(1.0) } else { 0.0 };
    (read * READ_SHARE + combine * (1.0 - READ_SHARE)) * 100.0
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p athenaeum-core --lib api::masters::tests::build_percent`
Expected: 2 passed.

- [ ] **Step 5: Rewire the two closures in `run_build`**

In `run_build`, directly after `const PROGRESS_THROTTLE: … = …from_millis(300);` add the shared counters:

```rust
    // Running maxima behind `build_percent` (review 2026-09-06 F1): each
    // callback advances its own counter and reads the other's, so whichever
    // stage emits, the percent is computed from the furthest point BOTH
    // phases have reached — one bar, never two scales in one cell.
    let max_bytes_done = std::sync::atomic::AtomicU64::new(0);
    let max_rows_done = std::sync::atomic::AtomicUsize::new(0);
    let rows_total_seen = std::sync::atomic::AtomicUsize::new(0);
```

Replace the body of `on_band` from its first line through the `let percent = …;` block (keep the existing comment about "Bytes, not bands" if you like, but replace the `percent` computation) with:

```rust
    let on_band = |current: usize, total: usize, bytes_read_so_far: u64, bytes_total: u64| {
        // Running max, and the duplicate filter (review 2026-09-06 F1): the
        // engine calls `on_band` once more at the END of every band, after
        // that band's combine, with the byte count its last per-frame tick
        // already carried. Emitting that would flip the stage word back to
        // "integrating" behind a "combining" tick that reported MORE
        // progress. No new bytes means there is nothing new to say.
        let prev = max_bytes_done.fetch_max(bytes_read_so_far, Ordering::Relaxed);
        if bytes_read_so_far <= prev {
            return;
        }
        let percent = build_percent(
            bytes_read_so_far,
            bytes_total,
            max_rows_done.load(Ordering::Relaxed),
            rows_total_seen.load(Ordering::Relaxed),
        );
```

Everything from `// The terminal tick (100% read) always bypasses the throttle` to the closing `};` of `on_band` stays exactly as it is (the `is_terminal` check, the throttle, the `emit_event` with `stage: "integrating"`).

Replace the start of `on_combine` through its `let percent = …;` line with:

```rust
    let on_combine = |current: usize, total: usize, bytes_done: u64, bytes_total: u64| {
        // Rows advance here; bytes are frozen for the whole combine (see
        // `EngineProgress::on_combine`'s doc), so the read share of the bar
        // holds and only the combine share moves. `current` can arrive
        // slightly out of order across rayon workers — the running max is
        // what feeds the percent, the event still carries the value as given.
        rows_total_seen.store(total, Ordering::Relaxed);
        max_rows_done.fetch_max(current, Ordering::Relaxed);
        let percent = build_percent(
            max_bytes_done.load(Ordering::Relaxed).max(bytes_done),
            bytes_total,
            max_rows_done.load(Ordering::Relaxed),
            total,
        );
```

Everything from `// Terminal bypass keyed on ROWS` to the closing `};` stays as it is (`stage: "combining"`).

Delete the now-stale sentence in `on_combine`'s old comment that said the rows percent is "the opposite of `on_band`'s bytes-first, bands-fallback order" — there is no bands fallback any more.

- [ ] **Step 6: Move the finished line below the cancel check (F3)**

In `run_build`, the block currently reads:

```rust
    log_build_finished(set_id, build_started_at.elapsed(), out.bytes_read, &out);

    // Fix wave item 1 (whole-branch review, CRITICAL): …
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(BuildStepError::Cancelled);
    }
```

Change it to:

```rust
    // Fix wave item 1 (whole-branch review, CRITICAL): …   ← keep the whole existing comment
    //
    // Review 2026-09-06 F3: this check sits ABOVE `log_build_finished` on
    // purpose — a build that is about to return `Cancelled` must not first
    // log "master build finished"; `run_master_build_thread` logs the
    // "master build cancelled" line for it instead.
    if cancel_flag.load(Ordering::Relaxed) {
        return Err(BuildStepError::Cancelled);
    }
    log_build_finished(set_id, build_started_at.elapsed(), out.bytes_read, &out);
```

- [ ] **Step 7: Update the event doc on the frontend**

In `src/types/helpers.ts`, inside `MasterBuildProgressEvent`, add a doc comment on `percent`:

```ts
  /** One monotonic 0-100 across the whole pixel phase: reading (bytes) is
   *  weighted 0.7, combining (rows) 0.3 — see `build_percent` in
   *  `athenaeum-core/src/api/masters.rs`. Never goes backwards between
   *  `'integrating'` and `'combining'`; `'writing'`/`'registering'` report 0. */
  percent: number;
```

- [ ] **Step 8: Run the module's tests and the type check**

Run: `cargo test -p athenaeum-core --lib api::masters:: && npx tsc --noEmit`
Expected: all pass, tsc silent.

- [ ] **Step 9: Commit**

```bash
git add crates/athenaeum-core/src/api/masters.rs src/types/helpers.ts
git commit -m "fix(masters): one monotonic progress percent per build, and log finished only after the cancel check"
```

---

### Task 2: Cancel and fail-fast inside the band readers; reuse the probe's file handle (F2, F6)

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (`probe_fits` ~216, `probe_bitpix` ~258, `probe_one` ~372, `open` ~409, `read_band` ~542, `read_band_with_progress` ~584, the Windows comment inside `FrameReader::read_exact_at` ~161–188)
- Modify: `crates/athenaeum-core/src/integration/engine.rs` (call sites at ~229, ~374, ~404, ~466)
- Test: `crates/athenaeum-core/src/integration/banded.rs` `mod tests`

**Interfaces:**
- Produces:
  - `pub fn open_with_cancel(paths: &[PathBuf], scratch_dir: &Path, concurrency: usize, cancel: &AtomicBool) -> Result<BandSource, IntegrationError>` — `open` keeps its signature and delegates with a never-raised flag.
  - `pub fn read_band_with_progress(&self, y0: usize, rows: usize, out: &mut BandPlanes, concurrency: usize, on_bytes: &(dyn Fn(u64) + Sync), cancel: &AtomicBool) -> Result<(), IntegrationError>` — one new trailing parameter. `read_band` keeps its signature.
  - Both return `Err(IntegrationError::Cancelled)` when the flag is raised; a read error from any worker raises a shared abort so sibling workers stop within one frame.
- Consumes: `IntegrationError::Cancelled` (exists).

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `banded.rs` (the module already has `f32_fixture`):

```rust
    /// Review 2026-09-06 F2: the read workers never looked at the caller's
    /// cancel flag, so a Cancel waited for the whole band — ~25 s on a 4 GiB
    /// band at 175 MB/s, and seen live (cancel 19:04:07, stop 19:04:13).
    #[test]
    fn read_band_stops_within_one_frame_of_a_cancel() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..6)
            .map(|i| f32_fixture(dir.path(), &format!("c{i}.fits"), 8, 8, move |_, _| i as f32))
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let mut planes = BandPlanes::new(&src);
        let cancel = AtomicBool::new(false);
        let frames_read = AtomicUsize::new(0);
        // Raise the flag from inside the first frame's tick: frame 2 must not be read.
        let on_bytes = |_: u64| {
            frames_read.fetch_add(1, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
        };
        let r = src.read_band_with_progress(0, 8, &mut planes, 1, &on_bytes, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)), "got {r:?}");
        assert_eq!(frames_read.load(Ordering::Relaxed), 1, "a cancel raised after frame 1 must stop before frame 2");
    }

    #[test]
    fn read_band_honours_a_cancel_under_concurrency() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..32)
            .map(|i| f32_fixture(dir.path(), &format!("p{i}.fits"), 8, 8, move |_, _| i as f32))
            .collect();
        let src = BandSource::open(&paths, dir.path(), 4).unwrap();
        let mut planes = BandPlanes::new(&src);
        let cancel = AtomicBool::new(true); // raised before the read starts
        let frames_read = AtomicUsize::new(0);
        let on_bytes = |_: u64| { frames_read.fetch_add(1, Ordering::Relaxed); };
        let r = src.read_band_with_progress(0, 8, &mut planes, 4, &on_bytes, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)), "got {r:?}");
        assert_eq!(frames_read.load(Ordering::Relaxed), 0, "no worker may read a frame past a raised flag");
    }

    #[test]
    fn open_returns_cancelled_when_the_flag_is_already_raised() {
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..8)
            .map(|i| f32_fixture(dir.path(), &format!("o{i}.fits"), 8, 8, move |_, _| i as f32))
            .collect();
        let cancel = AtomicBool::new(true);
        let r = BandSource::open_with_cancel(&paths, dir.path(), 4, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)), "got {:?}", r.err());
    }

    /// Review 2026-09-06 F6: `probe_fits` opened the file for its header and
    /// `probe_one` opened it AGAIN for the reader. The handle is reused now;
    /// this pins that a reader built from the probe's own handle still reads
    /// the right bytes at the right offset (the header read left the cursor
    /// past the header, which must not matter to a positional reader).
    #[test]
    fn a_reader_built_from_the_probe_handle_reads_the_data_section() {
        let dir = tempfile::tempdir().unwrap();
        let p = f32_fixture(dir.path(), "h.fits", 16, 4, |x, y| (y * 16 + x) as f32);
        let src = BandSource::open(&[p], dir.path(), 1).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band(2, 2, &mut planes, 1).unwrap();
        assert_eq!(planes.sample(0, 0), (2 * 16) as f32, "row 2 col 0");
        assert_eq!(planes.sample(0, 2 * 16 - 1), (3 * 16 + 15) as f32, "row 3 col 15");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p athenaeum-core --lib integration::banded::tests::read_band_stops integration::banded::tests::read_band_honours integration::banded::tests::open_returns_cancelled`
Expected: compile errors (`open_with_cancel` missing; `read_band_with_progress` takes 5 args, 6 given).

- [ ] **Step 3: Add the imports and the never-raised flag**

At the top of `banded.rs`, next to the existing `use` lines:

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// For callers with no cancellation source (single-frame precal / light-cal /
/// cosmetic reads, tests): the flag `open` and `read_band` pass on their
/// callers' behalf. Never raised.
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
```

- [ ] **Step 4: `probe_fits` hands back its handle; `probe_one` reuses it (F6)**

Change the signature and the two returns of `probe_fits`:

```rust
/// Scan primary-header blocks for END; harvest the handful of numeric cards
/// the direct reader needs. Returns the open handle alongside the info so the
/// caller can build its reader on it instead of opening the path a second
/// time (review 2026-09-06 F6: one open per frame is exactly the per-file
/// latency the parallel probe exists to amortize on a network mount). The
/// header read leaves the cursor past the header; every read a `FrameReader`
/// makes is positional, so that never matters. Returns None for anything that
/// should take the decode-and-spill fallback (never errors on odd files).
fn probe_fits(path: &Path) -> Option<(File, FitsInfo)> {
    let mut f = File::open(path).ok()?;
    …(body unchanged)…
    if info.naxis == 2 && info.naxis3 == 1 && ok_bitpix && info.w > 0 && info.h > 0 {
        Some((f, info))
    } else {
        None
    }
}

pub fn probe_bitpix(path: &Path) -> Option<i32> {
    probe_fits(path).map(|(_, i)| i.bitpix)
}
```

And `probe_one`:

```rust
fn probe_one(p: &Path) -> Result<ProbeOutcome, IntegrationError> {
    match probe_fits(p) {
        Some((file, info)) => Ok(ProbeOutcome::Fits(
            FrameReader::Fits {
                file,
                data_offset: info.data_offset,
                kind: plane_kind_for_bitpix(info.bitpix, info.bzero, info.bscale),
            },
            info.w, info.h,
        )),
        None => Ok(ProbeOutcome::NeedsSpill),
    }
}
```

Then rewrite the paragraph inside the `#[cfg(windows)]` arm of `FrameReader::read_exact_at` that begins "That invariant is narrower than "every read in this file" — `probe_fits`, a few lines up, opens its OWN short-lived `File`…" to say:

```rust
            // That invariant covers the one cursor-based read a `FrameReader`'s
            // handle ever sees: `probe_fits` reads the header with a plain
            // `read_exact` BEFORE the handle becomes a `FrameReader` (review
            // 2026-09-06 F6 reuses that handle instead of reopening the path).
            // From then on every read is positional, so where the cursor came
            // to rest is irrelevant. Concurrent calls on one handle are still
            // safe because each `seek_read` carries its own offset.
```

Keep the rest of that comment (the "Do NOT reintroduce a seek-based read" warning) as is.

- [ ] **Step 5: `open` → `open_with_cancel`**

Replace the `open` method with the pair:

```rust
    pub fn open(paths: &[PathBuf], scratch_dir: &Path, concurrency: usize) -> Result<BandSource, IntegrationError> {
        Self::open_with_cancel(paths, scratch_dir, concurrency, &NEVER_CANCELLED)
    }

    /// `open` with the caller's cancel flag (review 2026-09-06 F2): each probe
    /// worker checks it between files, the assembly loop checks it before
    /// every decode-and-spill (a spill decodes a whole frame, ~100 MB, on
    /// this thread), and a raised flag comes back as `Cancelled` — never as
    /// a half-built source.
    pub fn open_with_cancel(
        paths: &[PathBuf],
        scratch_dir: &Path,
        concurrency: usize,
        cancel: &AtomicBool,
    ) -> Result<BandSource, IntegrationError> {
```

Inside, keep the existing body with three insertions:

1. In the `workers == 1` loop and in each spawned worker's loop, before `*slot = Some(probe_one(p));`:

```rust
                if cancel.load(Ordering::Relaxed) { break; }
```

(the worker closure already `move`s `group`; `cancel` is a `&AtomicBool`, which is `Copy`, so it is captured by copy of the reference — nothing else changes.)

2. After the whole `if workers == 1 { … } else { … }` statement (i.e. after the `else` block that ends with `if panicked { return Err(…) }`), and before `let mut readers = …`:

```rust
        // A raised flag leaves probed-but-unassembled slots (and unprobed
        // `None`s) behind; report the cancel, not "never probed".
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
```

3. In the assembly loop's `ProbeOutcome::NeedsSpill` arm, as its first statement:

```rust
                    if cancel.load(Ordering::Relaxed) {
                        return Err(IntegrationError::Cancelled);
                    }
```

- [ ] **Step 6: `read_band` / `read_band_with_progress` take the flag; workers fail fast**

`read_band` delegates:

```rust
        self.read_band_with_progress(y0, rows, out, concurrency, &|_| {}, &NEVER_CANCELLED)
```

`read_band_with_progress` gains the trailing parameter `cancel: &AtomicBool` and its doc comment gains one paragraph:

```rust
    /// `cancel` (review 2026-09-06 F2) is checked by every worker between
    /// frames, so a Cancel takes effect within one frame's read rather than
    /// one band's (a band can be gigabytes). A worker that hits a read error
    /// raises a shared abort so its siblings stop within one frame too — the
    /// caller is about to discard the band either way. A raised cancel comes
    /// back as `Cancelled` unless a real error happened first; the error wins,
    /// because it is the one the operator has to act on.
```

The `workers == 1` fast path becomes:

```rust
        if workers == 1 {
            for (reader, buf) in self.readers.iter().zip(out.bufs.iter_mut()) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(IntegrationError::Cancelled);
                }
                let bpp = reader.kind().bytes_per_sample();
                buf.resize(rows * w * bpp, 0u8);
                reader.read_exact_at(buf, (y0 * w * bpp) as u64)?;
                on_bytes((rows * w * bpp) as u64);
            }
            return Ok(());
        }
```

The parallel path: declare the abort flag before `thread::scope` and check both flags per frame:

```rust
        let abort = AtomicBool::new(false);
        let abort = &abort;
        let mut errors: Vec<IntegrationError> = Vec::new();
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for group in groups {
                handles.push(scope.spawn(move || -> Result<(), IntegrationError> {
                    for (reader, buf) in group {
                        if cancel.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        let bpp = reader.kind().bytes_per_sample();
                        buf.resize(rows * w * bpp, 0u8);
                        if let Err(e) = reader.read_exact_at(buf, (y0 * w * bpp) as u64) {
                            abort.store(true, Ordering::Relaxed);
                            return Err(e.into());
                        }
                        on_bytes((rows * w * bpp) as u64);
                    }
                    Ok(())
                }));
            }
            …(join loop unchanged)…
        });
        if !errors.is_empty() {
            …(unchanged: warn the extras, return the first)…
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        Ok(())
```

- [ ] **Step 7: Thread the flag through the engine**

In `crates/athenaeum-core/src/integration/engine.rs`:

- `integrate_bias_like_inner` and `integrate_flat_inner`: `BandSource::open(paths, scratch_dir, io.read_concurrency)?` → `BandSource::open_with_cancel(paths, scratch_dir, io.read_concurrency, cancel)?` (both functions already have `cancel: &AtomicBool` in scope).
- `run_banded` (~229) and the flat pass-1 loop (~466): `src.read_band_with_progress(…, &on_bytes)?` → `src.read_band_with_progress(…, &on_bytes, cancel)?`.

The existing post-read and post-combine cancel checks in the engine stay: they are what turns a cancel that landed AFTER the last frame's read into `Cancelled` before the combine starts.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p athenaeum-core --lib integration::`
Expected: all pass, including the four new ones and the pre-existing `cancel_flag_aborts_run` (its pre-raised flag now surfaces from `open_with_cancel`, same `Err(Cancelled)`).

Also run the light-cal / cosmetic / masters callers, which use `open` / `read_band` unchanged: `cargo test -p athenaeum-core --lib calibration_library:: api::masters::`.

- [ ] **Step 9: Commit**

```bash
git add crates/athenaeum-core/src/integration/banded.rs crates/athenaeum-core/src/integration/engine.rs
git commit -m "fix(integration): honour cancel inside the probe and read workers, fail fast on a read error, reuse the probe's file handle"
```

---

### Task 3: The VNG gate is taken by the host, before the image semaphore (F4)

**Files:**
- Modify: `crates/athenaeum-core/src/rustafits_processor/mod.rs` (`VNG_GATE` ~line 24, `process_fits_to_jpeg` ~lines 110–160, tests at the bottom)
- Modify: `crates/athenaeum-tauri/src/commands_rustafits.rs` (`read_fits_image_bytes`, between the first cache check and `let sem = …`)
- Modify: `crates/athenaeum-web/src/routes/images.rs` (`get_frame_preview`, between the cache check and `let sem = …`)
- Test: `crates/athenaeum-core/src/rustafits_processor/mod.rs` `mod tests`

**Interfaces:**
- Produces (core, `render`-gated module as before):
  - `pub static VNG_GATE: tokio::sync::Mutex<()>`
  - `pub fn needs_vng_gate(path: &Path, resolution: Resolution) -> bool` — header-only.
  - `process_fits_to_jpeg` no longer locks anything; its doc says the caller holds the gate for the renders `needs_vng_gate` names.
- Consumes: `crate::fits_parser::parse_fits(path, 0)` / `parse_xisf(path, 0)` returning `models::Frame` with `bayerpat: Option<String>` (header-only readers the scanner uses).
- Lock order, both hosts: **gate, then permit — never the reverse.** A permit holder never waits for the gate, so there is no cycle.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `rustafits_processor/mod.rs` (the module already has `write_cfa_fits`):

```rust
    /// Review 2026-09-06 F4: the gate moved out of the render and in front of
    /// the host's semaphore; the host decides from the header alone. Only a
    /// full-resolution render of a CFA frame needs it.
    #[test]
    fn needs_vng_gate_only_for_cfa_at_full() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfa = dir.path().join("cfa.fits");
        write_cfa_fits(&cfa, 16, 16);
        let mono = dir.path().join("mono.fits");
        write_fits_f32(&mono, 16, 16, 1, &vec![1.0f32; 256], &[]).unwrap();

        assert!(needs_vng_gate(&cfa, Resolution::Full), "CFA at Full is the VNG render");
        assert!(!needs_vng_gate(&cfa, Resolution::Preview), "preview keeps the super-pixel path");
        assert!(!needs_vng_gate(&cfa, Resolution::Thumbnail));
        assert!(!needs_vng_gate(&mono, Resolution::Full), "mono must never queue behind a colour render");
        assert!(
            needs_vng_gate(&dir.path().join("missing.fits"), Resolution::Full),
            "an unreadable header errs towards taking the gate"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib rustafits_processor::tests::needs_vng_gate`
Expected: compile error, `needs_vng_gate` not found.

- [ ] **Step 3: Replace the gate and add the probe in core**

Replace the `static VNG_GATE: Mutex<()> = Mutex::new(());` declaration and its doc comment with:

```rust
/// Serializes full-resolution gradient debayers.
///
/// One such render allocates twelve bytes per pixel of planar RGB — measured at
/// 547 MB peak RSS on a 6248x4176 one-shot-colour frame — and already saturates
/// the machine on its own (5.84 s of CPU inside 0.79 s of wall time). Running
/// several at once therefore multiplies the memory peak without buying
/// throughput: on that frame, five in parallel took 3.40 s against 3.99 s one
/// after another, for five times the peak.
///
/// Held by the HOST, before its image semaphore, never inside the render
/// (review 2026-09-06 F4): both hosts bound concurrent renders with one
/// `image_semaphore` shared by every thumbnail, preview and full request. A
/// gate taken after the permit parked N-1 permits here during a
/// full-resolution colour prefetch and starved every unrelated request behind
/// the whole VNG backlog. Lock order is gate → permit everywhere; a permit
/// holder never waits for the gate, so there is no cycle. Async so a waiting
/// request parks its task, not a runtime thread. [`needs_vng_gate`] says
/// whether a given request must take it.
pub static VNG_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Whether rendering `path` at `resolution` is a full-resolution gradient
/// debayer and must hold [`VNG_GATE`]. Header-only — a FITS primary header
/// or an XISF XML header, never the pixels — through the same readers the
/// scanner uses, so the answer is what the catalog would say. Errs towards
/// `true` when the header cannot be read: the render will fail loudly on its
/// own, and taking the gate for a broken file costs one serialization, not
/// gigabytes.
pub fn needs_vng_gate(path: &Path, resolution: Resolution) -> bool {
    if resolution != Resolution::Full {
        return false;
    }
    let is_xisf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"));
    let bayerpat = if is_xisf {
        crate::fits_parser::parse_xisf(path, 0).map(|f| f.bayerpat)
    } else {
        crate::fits_parser::parse_fits(path, 0).map(|f| f.bayerpat)
    };
    match bayerpat {
        Ok(pat) => pat.is_some_and(|p| !p.trim().is_empty()),
        Err(error) => {
            tracing::debug!(path = %path.display(), error = %error, "header probe failed — taking the VNG gate defensively");
            true
        }
    }
}
```

Remove `Mutex` from the `use std::sync::{Arc, Mutex};` line (keep `Arc`).

In `process_fits_to_jpeg`, delete these two statements and the comment above them:

```rust
    let vng_render = resolution == Resolution::Full && meta.bayer_pattern != BayerPattern::None;
    let _gate = vng_render.then(|| VNG_GATE.lock().unwrap_or_else(|e| e.into_inner()));
```

and drop `BayerPattern` from the `use astroimage::{BayerPattern, ImageConverter};` import. Keep the `read_raw` / `process_data` split and its `pool.install` comment — the pool reasoning still holds. Replace the sentence in the function's doc "That render is the one serialized by [`VNG_GATE`]." with "The host serializes that render by holding [`VNG_GATE`] around this call when [`needs_vng_gate`] says so; this function takes no lock itself."

- [ ] **Step 4: Run the core tests**

Run: `cargo test -p athenaeum-core --lib rustafits_processor::`
Expected: 3 passed (the two existing + the new one).

- [ ] **Step 5: Desktop host takes the gate before the permit**

In `crates/athenaeum-tauri/src/commands_rustafits.rs`, in `read_fits_image_bytes`, insert directly above `// Slow path: acquire semaphore for actual processing`:

```rust
    // Gate BEFORE permit, never the reverse (see `VNG_GATE`'s doc): a
    // full-resolution colour render waits here holding nothing, so the image
    // semaphore keeps serving thumbnails and previews meanwhile. Cache hits
    // above never reach this line, so they never probe a header either.
    let _vng_gate = if rustafits_processor::needs_vng_gate(&path_buf, res) {
        Some(rustafits_processor::VNG_GATE.lock().await)
    } else {
        None
    };
```

`_vng_gate` lives until the function returns, i.e. through the `block_in_place` render and the cache insert.

- [ ] **Step 6: Web host mirrors it**

In `crates/athenaeum-web/src/routes/images.rs`, in `get_frame_preview`, insert directly above `let sem = state.image_semaphore.read().unwrap().clone();`:

```rust
    // Gate BEFORE permit, never the reverse (see `VNG_GATE`'s doc in core):
    // a full-resolution colour render waits here holding nothing, so the
    // image semaphore keeps serving thumbnails and previews meanwhile.
    let res = Resolution::from_string(&resolution_str);
    let path_buf = PathBuf::from(&file_path);
    let _vng_gate = if rustafits_processor::needs_vng_gate(&path_buf, res) {
        Some(rustafits_processor::VNG_GATE.lock().await)
    } else {
        None
    };
```

and delete the later duplicate `let path_buf = PathBuf::from(&file_path);` and `let res = Resolution::from_string(&resolution_str);` lines (they now exist above). The guard is held across the `spawn_blocking(...).await`, so the render runs under it.

- [ ] **Step 7: Build both hosts and run the route/command tests**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web && cargo test -p athenaeum-web`
Expected: builds; web tests green.

- [ ] **Step 8: Update the spec's gate section**

In `docs/superpowers/specs/2026-09-06-blink-full-resolution-vng-design.md` §4.2, after the paragraph that begins "`VNG_GATE` is a `static std::sync::Mutex<()>` in this module.", add:

```markdown
**Amended 2026-09-06 (review F4).** The gate moved out of `process_fits_to_jpeg`
and in front of the hosts' `image_semaphore`: taken inside the render, after
the permit, it parked N−1 permits during a full-resolution colour prefetch and
starved every thumbnail/preview request that shares the semaphore. It is now
`pub static VNG_GATE: tokio::sync::Mutex<()>`, and `needs_vng_gate(path,
resolution)` — a header-only probe through the scanner's own FITS/XISF readers
— tells the host whether to take it. Lock order is gate → permit everywhere,
so no cycle. A mono frame at `Full` still never takes it. Waiters now hold
nothing at all (not even the raw pixel buffer), which also retires the
"waiters hold only the raw buffer" caveat above.
```

- [ ] **Step 9: Commit**

```bash
git add crates/athenaeum-core/src/rustafits_processor/mod.rs crates/athenaeum-tauri/src/commands_rustafits.rs crates/athenaeum-web/src/routes/images.rs docs/superpowers/specs/2026-09-06-blink-full-resolution-vng-design.md
git commit -m "fix(render): take the VNG gate in the host before the image semaphore, decided from the header alone"
```

---

### Task 4: One resolver clamps `blink.memory_cache_max_mb` everywhere (F5)

**Files:**
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (new free function + consts near the other `pub fn`s of the module, outside `impl SettingsManager`; tests in the module's `mod tests`)
- Modify: `crates/athenaeum-tauri/src/lib.rs:173-177`, `crates/athenaeum-tauri/src/commands/settings.rs:47-49`
- Modify: `crates/athenaeum-web/src/main.rs:168-172`, `crates/athenaeum-web/src/routes/settings.rs:79-81`

**Interfaces:**
- Produces: `pub const BLINK_MEMORY_CACHE_MAX_MB_MIN: usize = 64;`, `pub const BLINK_MEMORY_CACHE_MAX_MB_MAX: usize = 16384;`, `pub fn resolve_blink_memory_cache_max_mb(raw: Option<&str>) -> usize` in `athenaeum_core::settings`.
- Consumes: `defaults::BLINK_MEMORY_CACHE_MAX_MB` (`"512"`), `keys::BLINK_MEMORY_CACHE_MAX_MB`.

- [ ] **Step 1: Write the failing test**

In `crates/athenaeum-core/src/settings/mod.rs` `mod tests`:

```rust
    /// Review 2026-09-06 F5: the byte budget had no backend clamp — a `0`
    /// from a script or a hand edit turned the cache into a single entry with
    /// no error and no log. One resolver, used at all four apply sites.
    #[test]
    fn blink_cache_max_mb_resolves_default_and_clamps() {
        assert_eq!(resolve_blink_memory_cache_max_mb(None), 512, "absent → default");
        assert_eq!(resolve_blink_memory_cache_max_mb(Some("abc")), 512, "unparseable → default");
        assert_eq!(resolve_blink_memory_cache_max_mb(Some("0")), BLINK_MEMORY_CACHE_MAX_MB_MIN, "0 must not mean one entry");
        assert_eq!(resolve_blink_memory_cache_max_mb(Some("999999")), BLINK_MEMORY_CACHE_MAX_MB_MAX);
        assert_eq!(resolve_blink_memory_cache_max_mb(Some(" 1024 ")), 1024, "whitespace tolerated");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p athenaeum-core --lib settings::tests::blink_cache_max_mb`
Expected: compile error, function not found.

- [ ] **Step 3: Add the resolver**

In `crates/athenaeum-core/src/settings/mod.rs`, at module level (not inside `impl SettingsManager`), after the `keys` module:

```rust
/// Bounds for `blink.memory_cache_max_mb`. The Settings page validates the
/// same 64..=16384 window; this is the backend's own copy, so a value that
/// reached the row another way (a script, an older client, a hand edit)
/// degrades instead of turning the preview cache into a single entry (`0`)
/// or an unbounded one.
pub const BLINK_MEMORY_CACHE_MAX_MB_MIN: usize = 64;
pub const BLINK_MEMORY_CACHE_MAX_MB_MAX: usize = 16384;

/// Resolve `blink.memory_cache_max_mb` from its raw stored text: absent or
/// unparseable falls back to the default (logged when a value was present),
/// then clamps. One function for the four sites that apply it — the two
/// startup readers and the two `set_setting` handlers — so they cannot drift
/// (review 2026-09-06 F5).
pub fn resolve_blink_memory_cache_max_mb(raw: Option<&str>) -> usize {
    let mb = match raw.and_then(|s| s.trim().parse::<usize>().ok()) {
        Some(v) => v,
        None => {
            if let Some(raw) = raw {
                tracing::warn!(
                    key = keys::BLINK_MEMORY_CACHE_MAX_MB,
                    value = %raw,
                    "unparseable setting value — using the default"
                );
            }
            defaults::BLINK_MEMORY_CACHE_MAX_MB
                .parse()
                .expect("BLINK_MEMORY_CACHE_MAX_MB default is numeric")
        }
    };
    mb.clamp(BLINK_MEMORY_CACHE_MAX_MB_MIN, BLINK_MEMORY_CACHE_MAX_MB_MAX)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p athenaeum-core --lib settings::tests::blink_cache_max_mb`
Expected: 1 passed.

- [ ] **Step 5: Use it at the four sites**

`crates/athenaeum-tauri/src/lib.rs` — replace

```rust
                    let cache_max_mb: usize = db::get_setting(&conn, settings::keys::BLINK_MEMORY_CACHE_MAX_MB)
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(512);
```

with

```rust
                    let cache_max_mb = settings::resolve_blink_memory_cache_max_mb(
                        db::get_setting(&conn, settings::keys::BLINK_MEMORY_CACHE_MAX_MB)
                            .ok()
                            .flatten()
                            .as_deref(),
                    );
```

`crates/athenaeum-web/src/main.rs` — the same replacement for its `cache_max_mb` binding (it reads through `db::get_setting(&db.conn(), …)`).

`crates/athenaeum-tauri/src/commands/settings.rs` — replace

```rust
        let mb: usize = value.parse().unwrap_or(512);
```

with

```rust
        let mb = settings::resolve_blink_memory_cache_max_mb(Some(&value));
```

`crates/athenaeum-web/src/routes/settings.rs` — the same with `Some(&args.value)`.

- [ ] **Step 6: Build both hosts**

Run: `cargo build -p athenaeum-tauri -p athenaeum-web && cargo test -p athenaeum-web`
Expected: clean build, web tests green.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/settings/mod.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-tauri/src/commands/settings.rs crates/athenaeum-web/src/main.rs crates/athenaeum-web/src/routes/settings.rs
git commit -m "fix(settings): clamp blink.memory_cache_max_mb on the backend through one resolver"
```

---

### Task 5: Gates, ledger, merge

**Files:**
- Modify: `docs/superpowers/open-items.md` (the two 2026-09-06 sections)
- Modify: `docs/superpowers/specs/2026-09-06-integration-throughput-design.md` (§7 D6)

- [ ] **Step 1: Run every gate on the branch**

```bash
cargo build --workspace
cargo test --workspace
cargo check -p athenaeum-core --no-default-features
npx tsc --noEmit
```

Expected: all green. If `cargo test --workspace` reports a failure anywhere in `integration::`, `api::masters::`, `rustafits_processor::` or `settings::`, that is this plan's regression — fix it in the task that owns the file, never by weakening the test.

- [ ] **Step 2: Record the amendments**

In `docs/superpowers/specs/2026-09-06-integration-throughput-design.md`, at the end of §7 (D6), add:

```markdown
**Amended 2026-09-06 (review F1–F3).** `percent` on `master-build-progress`
is ONE monotonic number for the whole pixel phase — bytes read weighted 0.7,
rows combined 0.3 (`api::masters::build_percent`, running maxima of both
counters) — because a bytes percent during `integrating` and a rows percent
during `combining` in the same cell went backwards at every band boundary.
The engine's end-of-band `on_band` call, which repeats the last per-frame
byte count, is filtered at the emitter so the stage word never flips back.
The read workers and the probe stage now check the caller's cancel flag
between frames (`BandSource::open_with_cancel`,
`read_band_with_progress(.., cancel)`), so a Cancel takes effect within one
frame's read rather than one band's; a worker's read error aborts its
siblings. "master build finished" is logged only after the post-engine
cancel re-check.
```

In `docs/superpowers/open-items.md`, under "### Integration throughput (2026-09-06)", add one bullet:

```markdown
- After the 2026-09-06 review fixes: start a 100-frame build, press Cancel
  mid-read, and confirm the row stops within a couple of seconds (it used to
  wait for the whole band, ~6 s on the owner's 30-frame dark, ~25 s on a
  100-frame set) and that the log shows only "master build cancelled" for
  that set, no "finished". Watch the percent on a two-band build (any
  100-frame set on the 16 GB machine): it must climb without ever stepping
  back across the `integrating` → `combining` boundary.
```

Under "### Blink full-resolution VNG (2026-09-06)", add:

```markdown
- After the 2026-09-06 review fixes: with Image Resolution at **Full** and an
  OSC set buffering in Blink, open the file browser and hover thumbnails —
  they must render while the VNG prefetch is still running (the gate no
  longer sits inside the shared image semaphore).
```

- [ ] **Step 3: Commit the ledger**

```bash
git add docs/superpowers/open-items.md docs/superpowers/specs/2026-09-06-integration-throughput-design.md
git commit -m "docs: record the 2026-09-06 review fixes in the specs and the open-items ledger"
```

- [ ] **Step 4: Merge and push**

Use `superpowers:finishing-a-development-branch`. Expected outcome: `git checkout main && git merge --ff-only fix/review-wave-2026-09-06`, then `git push all main` (both remotes). Push only if the owner has said so in the session; otherwise stop after the merge and report.
