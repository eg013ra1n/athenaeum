# Integration throughput — design

**Date:** 2026-09-06
**Research:** `docs/superpowers/research/2026-09-06-master-integration-io-profiling.md`
**Goal:** make a master build read at the speed of the disk it is reading from,
without staging copies, without a second in-memory buffer of the data, and
without the operator having to guess what the app is doing.

Target on the profiling machine (16 GB, 7200 rpm SATA): the LDN 1272 batch
**13.5 min → ≤ 5 min**, and the 100-frame bias set **241.6 s → ≤ 40 s**.

---

## 1. Principles this design commits to

1. **Fewer, larger band passes — never a second copy.** Every measured
   alternative that adds memory (double-buffered bands, a transposed scratch
   file, an in-RAM frame cache) costs more than it returns, or violates the
   owner's "no copies into cache or memory" instruction. The one lever that
   pays is making the band buffers we *already allocate* big enough that the
   drive is read in a handful of sweeps instead of forty.
2. **The budget is a property of the machine, not of the source tree.** A
   compile-time 256 MiB is a permanent trade of speed for memory we are not
   short of. It becomes a resolved policy with an operator override.
3. **Less data materialized than today, not more.** Band buffers hold the
   source's own bytes; the widening to `f32` happens per row inside the
   parallel combine, where it is nearly free. For the common BITPIX 16 camera
   file this *halves* band memory, which buys twice the rows for the same
   budget.
4. **Nothing about the pixel maths changes.** `combine_pixel`, the rejection
   recipes, the non-finite accounting (`bad_samples_per_frame`,
   `all_bad_pixels`), the flat normalization and every stamped header card are
   untouched. The stronger, true claim: identity is safe **by construction**
   on every path, not merely hoped for and then spot-checked — the flat
   two-pass f64 sum order is fixed by the row walk (pass 1 always visits
   frames-then-rows-then-columns in the same order regardless of band count),
   each `PlaneKind` decode arm carries its own discriminating test (all six
   variants pinned individually), `PlaneKind::F32Le` (the decode-and-spill
   scratch format) carries no BZERO/BSCALE fields at all, so a scale/offset
   mismatch from a spill is unrepresentable in the type rather than merely
   untested, and the storage-class-derived read concurrency (D3/D3b) changes
   only which worker thread fills which frame's band buffer, never the
   buffer's contents or the order combine walks them in. Of the input
   formats this reasoning covers, only the `I16Be` path — by far the common
   camera case — carries an end-to-end MEASURED fingerprint
   (`a4f6bb5158714175`, reproduced across six cold runs); the others are
   covered by construction and by their own unit tests, not by a matching
   before/after fingerprint run.

## 2. D1 — the band budget becomes a resolved policy

New module `crates/athenaeum-core/src/integration/band_budget.rs`.

```
configured (integration.band_budget_mb)  0 = auto
   auto = clamp(total_ram_bytes / 4, MIN, MAX)
effective = max(chosen / compute.max_concurrent, MIN)
```

| Constant | Value | Why |
| ---- | ---- | ---- |
| `MIN_BUDGET_BYTES` | 256 MiB | today's constant — the policy can never make anything slower than it is now |
| `MAX_BUDGET_BYTES` | 8 GiB | 100 × 26 Mpx as `u16` is 5.2 GB; 8 GiB is where a large machine reaches a single band and therefore whole-file sequential reads |
| `FALLBACK_BUDGET_BYTES` | 1 GiB | used when the RAM probe fails; measured 101.2 s vs 241.6 s, and any machine running a 26 Mpx pipeline has ≥ 8 GB |

`total_ram_bytes()` is a small `#[cfg]` helper, **not** a new dependency tree:

- **macOS/BSD** — `libc::sysctlbyname("hw.memsize")`. `libc` is already a
  `cfg(unix)` dependency of `athenaeum-core`.
- **Linux** — `MemTotal:` from `/proc/meminfo`, **and** `min()` with the
  container limit when one is present: `/sys/fs/cgroup/memory.max` (v2, the
  literal `max` meaning unlimited) or
  `/sys/fs/cgroup/memory/memory.limit_in_bytes` (v1). This is load-bearing for
  the Docker/web build — `/proc/meminfo` reports the **host's** RAM inside a
  container, so without the cgroup read a 2 GB container would size an 8 GiB
  budget and be OOM-killed.
- **Windows** — `GlobalMemoryStatusEx`, adding
  `windows-sys = { version = "0.59", features = ["Win32_System_SystemInformation"] }`
  under `[target.'cfg(windows)'.dependencies]`. `windows-sys` 0.59 is already
  in `Cargo.lock` transitively, so this costs no new build time.
- Anything else — `None` ⇒ `FALLBACK_BUDGET_BYTES`.

Dividing by `compute.max_concurrent` is what keeps the policy safe if the
operator raises the compute queue's ceiling: two admitted builds must not each
believe they own a quarter of RAM.

**Setting:** `integration.band_budget_mb`, default `"0"`. On read, `0` passes
through as the auto sentinel; any other value is clamped to `256..=16384` MB —
the lower bound equals `MIN_BUDGET_BYTES` deliberately: `per_job_budget` floors
every resolved budget there anyway, so advertising a smaller minimum would be
promising a setting the backend refuses to honour —
the same defense-in-depth clamp `get_compute_max_concurrent` and
`get_sync_max_concurrent_receives` apply, so a value that reached the row by a
direct DB edit or a botched import degrades instead of OOM-ing the app.

## 3. D2 — band planes hold source bytes, decoded per row

`BandSource`'s output type changes from `Vec<Vec<f32>>` to a `BandPlanes` value
that owns one raw byte buffer per frame plus that frame's decode parameters
(BITPIX, BZERO, BSCALE, or "little-endian f32 scratch" for the
decode-and-spill fallback).

```rust
pub struct BandPlanes { /* per frame: Vec<u8> + PlaneKind; plus rows, width */ }

impl BandPlanes {
    pub fn new(src: &BandSource) -> Self;
    pub fn frame_count(&self) -> usize;
    pub fn rows(&self) -> usize;

    /// One sample, decoded. For cold paths (flat pass 1, single-frame reads).
    #[inline] pub fn sample(&self, frame: usize, idx: usize) -> f32;

    /// All frames' samples for one row of the band, into `dst` of len n*width,
    /// frame-major (`dst[i * width + x]`). The hot path: a tight per-frame
    /// typed loop the optimizer can vectorize, called once per row worker.
    pub fn decode_row_into(&self, row_in_band: usize, dst: &mut [f32]);

    /// One frame's whole band into `dst` of len rows*width.
    pub fn decode_frame_into(&self, frame: usize, dst: &mut [f32]);
}
```

`band_rows_for_budget` stops being a free function that assumes `f32` and
becomes a method that knows its own frames' sample widths:

```rust
impl BandSource {
    /// Rows per band whose working set fits `budget_bytes`: the sum of every
    /// frame's own bytes-per-row, plus one f32 output row and one f32 row of
    /// per-worker decode scratch. Floor of 1 — the floor must never override
    /// the budget (2026-08-02 audit I5).
    pub fn band_rows_for_budget(&self, budget_bytes: usize) -> usize;
}
```

**Memory effect, 100 frames × 6248 wide, 4 GiB budget:**
today 105 rows/band from 256 MiB; after D1 alone 1684 rows (3 bands); after D1
+ D2 the same 4 GiB holds ~3300 rows (2 bands) because `u16` rows are half the
size of `f32` rows.

**D2 adds zero new allocations.** The per-pixel gather in `run_banded` already
builds a small `column: Vec<f32>` of n samples per row; it keeps doing exactly
that, filling it with `planes.sample(i, idx)` instead of `frame[idx]`. Nothing
is materialized that was not materialized before, and the multi-GB `f32` band
it replaces is simply never allocated.

The obvious next optimization — decoding a whole row of every frame into a
per-worker `n × width` f32 scratch so the inner loop reads plain floats and
the decode vectorizes — is **deliberately not in this cycle**. The combine is
~16 % of the post-D1 run and the per-sample match is monomorphic per frame, so
the win is speculative; `BandPlanes::decode_row_into` exists in the API for
whoever measures it, and `decode_frame_into` is used today by the two
single-frame full-plane readers.

## 4. D3 — positional reads, with a class-derived number of them in flight

`read_band` takes `&self` (not `&mut self`) and fills the planes with
`FileExt::read_exact_at` on unix / a `seek_read` loop on Windows. No cursor, so
no `&mut`, so several reads can be outstanding at once.

**How many is not a CPU question**, and this is the part that decides the
architecture rather than a constant. The frames a build reads live on one of
three things — a spinning disk, an SSD, or a network mount — and only the third
inverts anything:

- A **network** mount is latency-bound, not seek-bound. The link is filled by
  the number of outstanding requests, and that number needs to be able to
  exceed the machine's core count. A rayon pool cannot express that: it caps
  parallelism at its own thread width, which is `available_parallelism()`.
- **Local** storage, rotating or solid, has no such need. And crucially there
  is no measured case for giving a spinning disk *fewer* readers than an SSD:
  the profiled 7200 rpm SATA drive got **faster** with 10-way concurrency
  (research §3.1), not slower.

So: **two classes, not three**, and reads leave the CPU pool. `read_band` takes
its parallelism as an argument and runs on scoped OS threads — exact,
pool-independent, ~10-20 us to spawn against a band read measured in seconds,
and safe to call from inside a `pool.install(..)` since they are not rayon
workers. The combine keeps the pool; the read stops borrowing it.

Dropping the rotational/solid split also drops the one detection that is
genuinely hard: answering "is this rotating" on macOS needs IOKit, for a
verdict nothing would act on. What remains is a single reliable flag per
platform (D3b).

No extra buffers either way: the planes are allocated once per build and
refilled per band.

## 4a. D3b — storage class is a deterministic OS property, never a timing probe

`integration::storage_class` classifies each **distinct parent directory** of
the frame list (normally one) and returns `Network` if any is:

| Platform | Probe | Network when |
| ---- | ---- | ---- |
| macOS | `statfs(2)` | `MNT_LOCAL` clear — the kernel's own answer, set for every physical/attached volume, clear for nfs/smbfs/afpfs/webdav |
| Linux | `statfs(2)` | `f_type` in an explicit magic table (NFS, CIFS/SMB1, SMB2/3, old smbfs, 9P, CephFS, AFS, Lustre) |
| Windows | path shape + `GetDriveTypeW` | a UNC path, or the volume root reports `DRIVE_REMOTE` |
| anything else | — | never — unknown is `Local` |

Failure and non-recognition both fall to `Local`, which is the conservative
direction: it never asks for more readers than there are cores. A set spanning
a NAS and a local disk resolves to `Network`, because the extra readers are
what the network members need and the local members were measured tolerating
them.

**Never a runtime timing probe.** A probe is non-deterministic, would spend its
first bands on a knowingly wrong setting, depends on what else is touching the
disk at that second, and is close to untestable — all to auto-tune the smaller
of the two levers, while the larger one (band size) is already fixed by policy.

Policy, with `integration.read_concurrency` (`0` = auto) as the override:

| Class | Readers in flight |
| ---- | ---- |
| Local | the CPU pool's width (`available_parallelism().min(16)`) |
| Network | `clamp(cores * 2, 8, 32)` — floor so a 4-core box still fills a LAN mount, ceiling so a slow uplink is not flooded |

**Honestly labelled:** the *local* number is measured (research §3.1). The
*network* bounds are reasoned from how NFS/SMB behave, not measured — no
network measurement exists yet. It is an open item, and the setting is the
escape hatch until it lands. The build logs `storage` and `read_threads` so
that measurement is interpretable when someone takes it.

A network mount also makes `BandSource::open` matter: its per-file header probe
is an `open` plus a couple of 2880-byte reads, so 100 frames cost 100 serial
round trips before a pixel is read (0.55 s even locally). It gets the same
concurrency, with frame order preserved — `bad_samples_per_frame` is indexed by
it.

## 5. D4 — deliberately NOT done: overlapping read and combine

`run_banded` will still alternate read and combine. §7 of the research shows
why: at the post-D1/D2 band count the combine is ~16 % of the run, and buying
overlap costs a second set of band planes, i.e. halving the budget, which the
§3.1 curve says costs more than 16 %. **The cure for the alternation is fewer
bands, and D1+D2 deliver it.** If a future machine profile shows compute
dominating (all-NVMe, 64 GB, small frame counts), revisit with a measurement,
not by assumption.

## 6. D5 — flats keep their two passes

`integrate_flat_inner` reads the central third for per-frame means, then reads
the whole frame to combine. The mean is an input to the combine, so it cannot
be folded into a single pass without holding the data. It inherits the new
budget and the new planes and gets no structural change. Its pass-1 inner loop
stays scalar: it touches 1/9 of the pixels, and the research measured the flat
sets (10 frames each) as a rounding error next to the 100-frame bias.

## 7. D6 — the operator can see what is happening

Three changes, all small, closing the gap the research recorded in §8.

- **Log the lifecycle.** `info!` at build start with `set_id`, `imagetyp`,
  `count`, `recipe`, `budget_mb`; `info!` at finish with `duration_ms`,
  `read_ms`, `combine_ms`, `read_mb_s`, `band_rows`, `bands` — the last two
  move to the FINISH line, not the start one, because neither is knowable
  until the engine has actually resolved and run the band geometry. Canonical
  field names only (`duration_ms`, `count`, `outcome` per the logging spec's
  dictionary — `frames` is not in that dictionary, hence `count` at the start
  line, not `frames`); the new ones — `band_rows`, `bands`, `budget_mb`,
  `read_ms`, `combine_ms`, `read_mb_s` — are added to that dictionary in the
  same change.
- **Carry bytes in the progress event.** `MasterBuildProgressEvent` gains
  `bytes_done` / `bytes_total` alongside the existing `current`/`total`/
  `percent`, mirrored in `src/types/helpers.ts`. Same snake_case on both sides
  (the struct has no `rename_all`; do not add one).
- **Stop discarding the percentage.** `CalibrationHierarchyView.tsx:129-133`
  reduces the whole event to `s.phase`; it keeps the `BuildState` instead, and
  the calibration row renders `stage` + `percent` for a set that is building.
  The sidebar `ComputeQueueIndicator` is untouched — its entries carry no
  calibration-set id, so joining progress to them is a separate change.

## 8. Out of scope, recorded so it is not lost

- **`light_cal.rs` / `cosmetic.rs` keep a fixed 256 MiB budget.** They run with
  n = 1–3 frames, which already yields 1–2 bands, so the budget is not their
  bottleneck. They must still migrate to `BandPlanes` because the type changes
  under them — that migration is behaviour-preserving and is part of this
  cycle; threading the *resolved* budget into them is not.
- **`light_cal.rs`'s per-pixel loop is serial** (`light_cal.rs:398`, no rayon
  at all) — the calibrated-lights export's real cost. Its own cycle.
- **`ComputeQueueEntry` carries no subject id**, so the sidebar cannot show a
  per-set percentage. Its own change.
- **The band budget does not vary by storage class.** It is already the most
  the machine can afford, and more of it helps every class — a network mount
  most of all, since bigger bands mean fewer round trips. Making two levers
  class-dependent instead of one would buy nothing and double what has to be
  reasoned about.
- **Rotational vs solid state is not distinguished** — see D3. Should a
  measurement ever show a spinning disk wanting *fewer* readers than the CPU
  pool, that is a third `StorageClass` variant and a row in the policy table,
  not a redesign: the seam is already in place.
- **The master is written to the calibration library root, which may itself be
  a network mount.** That is one ~104 MB sequential write per build against
  gigabytes of reading, so it is not touched here.

## 8a. Recorded for later: staging a network source to local scratch

Not in this cycle, and deliberately conditional — but written down with its
trigger, because if the owner's calibration ever moves onto the NAS this is the
right answer and re-deriving it would be waste.

**What it is.** When `StorageClass::Network` (D3b), read each source file ONCE,
sequentially, into a local scratch file, then band-read the scratch. One long
transfer per file instead of one round trip per file per band.

**Why it is not the answer today, and may not be even on a NAS.** Two reasons,
in order of how much they matter:

1. **Nothing is re-read.** The bands partition the frame height without
   overlap, so every byte of every source is read exactly once per integration.
   A cache accelerates *repeat* reads; this workload has none. What was lost on
   the profiled drive was seek locality within a single pass, and only the band
   size fixes that (§3.1: 40 bands to 3 is 241.6 s to 44.4 s, with no copy).
   Staging does not reduce the number of returns to a file — it reproduces them
   over the copy. The one genuine exception is a flat, whose pass 1 re-reads its
   central third; at ten frames a set that is noise.
2. **D1 and D2 have already taken most of what staging would have bought.** At
   the post-D1/D2 band count a network mount pays 2-3 round trips per file, not
   40. Staging only still pays if, at 2-3 bands, per-request latency STILL
   dominates one full sequential transfer plus a local write plus local band
   reads. That is an empirical question with a specific shape:

   ```
   stage when   S/L_seq + S/W_local + S/local_banded  <  S/L_banded(2-3 bands)
   ```

   No number exists for `L_banded` yet — three attempts to measure the owner's
   SMB share on 2026-09-06 all failed for different reasons (research §7b), so
   this stays a condition, not a decision.

**The shape it must take, if it is ever built.** Do NOT reuse the existing
decode-and-spill fallback (`banded.rs::spill_via_read_raw`) as the staging
mechanism, even though it superficially looks like one — it decodes the whole
frame into RAM via `ImageConverter::read_raw` and writes `f32`, so a BITPIX 16
source becomes a scratch file **twice the size** of the original (5.2 GB of bias
becomes 10.4 GB) and each frame transits a 104 MB heap allocation. Staging wants
the opposite: stream the data section's raw bytes to a local file verbatim,
keep the source's `BITPIX`/`BZERO`/`BSCALE`, and let the existing
`FrameReader::Fits` arm read the copy exactly as it reads the original. That is
a new, small `FrameReader` construction path, not a reuse.

**What it needs beyond the copy**, none of which the current engine has:
a scratch location that is not `std::env::temp_dir()` (the transfer folders
already set the precedent — `validate_transfer_dir` in `api::sync`); a
free-space check before starting, since a batch can stage 23 GB; removal on
cancel, on error and on a crashed previous run; and a staleness rule if a
staged copy is ever kept across builds rather than deleted at the end (the
catalog already has one in `db::disk_matches_row`). Every one of those is a
failure surface the current design does not carry, which is the second reason
it stays out until a measurement demands it.
- **Raising `compute.max_concurrent`** stays out: more builds on one spindle
  multiply seeks, and above 1 the batch's bias/darkflat → dark → flat ordering
  is already documented as best-effort.
- A stray `PixInsight's` in a doc comment at `light_cal.rs:714` violates the
  repo rule against naming other codebases in code and comments. Fix it in
  passing when that file is touched.

## 9. Acceptance

Measured with the checked-in `examples/band_profile.rs` harness and the
eviction protocol from research §2, on the profiling machine.

| Gate | Before | Target | Measured |
| ---- | ---- | ---- | ---- |
| 100-frame bias, cold, end to end | 233.1 s | ≤ 40 s | **37.7 s** pre-fix-wave; **40.4 / 40.8 s** on the final build — see note |
| 100-frame bias, read throughput | 23 MB/s | ≥ 150 MB/s | **175-178 MB/s**, stable across eight runs |
| 30-frame dark, cold, end to end | 11.8 s | ≤ 8 s | **11.7 s — the target was impossible, see below**; in the dev app 2026-09-06: 10.0 s (set 1682, read 6.7 s at 232 MB/s, combine 3.2 s) |
| LDN 1272 batch (11 sets, ~23 GB) | 13.5 min | ≤ 5 min | **≈ 3 min wall** in the dev app 2026-09-06 (19:02:43 → 19:05:39 by the `master build started/finished` log lines, including a ~12 s cancel-and-restart pause; the two 100-frame bias sets 42.7 s and 37.3 s at 177 / 227 MB/s; one 30-frame dark re-ran page-cache-hot after the cancel, so a fully cold batch is a few seconds longer) |

**The 30-frame dark target was wrong when it was written, and no implementation
could have met it.** 1.57 GB against the drive's measured 243 MB/s whole-file
ceiling is a 6.46 s read floor; plus the measured 2.75 s combine, the absolute
best possible total on this hardware is **9.21 s** — 1.21 s above the 8 s the
table demanded. The error was assuming the 5.3x from §3.1 generalised. It does
not: it applies where the band count was pathological, and this set's was not.
At 30 frames the old 256 MiB budget already bought 335-row bands, i.e. 4.2 MB
per read, enough to amortise seeks — the set was **already reading at 71 % of
the drive ceiling before the cycle began**. The cycle moved it 11.8 s -> 11.7 s,
under one percent, because there was nothing there to win. The pathology was
specific to high frame counts, where the same budget divided 100 ways gave
1.31 MB reads.

**On the 100-frame row.** The build measured at 37.66 s is the one before the
final fix wave. The final build measures 40.4 s and 40.8 s, i.e. 1-2 % over the
target. The whole difference sits in the combine phase, which the fix wave
touched by adding a per-row progress tick: it read 7.67 / 7.74 s before and
10.54 / 10.56 s after. That looks decisive and is not, for two reasons worth
recording rather than resolving by assertion. The combine's own spread across
the ten measurements taken earlier in this cycle was 6.08-10.05 s with no tick
present at all, so the post-wave pair sits barely above a range the phase
already occupied. And the tick's arithmetic cost is one relaxed `fetch_add` per
row — 8352 per run, against ~1257 us of real work per row, i.e. about 0.1 %
even at a microsecond per atomic, three orders of magnitude short of the 2.8 s
it would need to explain. Both post-wave samples are consecutive and share
machine state. **The honest statement is that the cause is not isolated**; the
numbers are recorded so a later reader can settle it with more samples rather
than inherit a guess.
| Master pixels | — | **byte-identical** to the pre-change build for the same inputs |
| SSD-backed scan root | not measured | open item — the class D3 expects to benefit most from concurrency |
| Network mount (SMB/NFS) | not measured | open item — the one class whose policy is reasoned, not measured |
| `cargo test --workspace` | green | green |
| `cargo check -p athenaeum-core --no-default-features` | green | green |
