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
   untouched. Byte-identical masters are an acceptance criterion, not a hope.

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
  `frames`, `recipe`, `band_rows`, `bands`, `budget_mb`; `info!` at finish with
  `duration_ms`, `read_ms`, `combine_ms`, `read_mb_s`. Canonical field names
  only (`duration_ms`, `count`, `outcome` per the logging spec's dictionary);
  the new ones — `band_rows`, `bands`, `budget_mb`, `read_ms`, `combine_ms`,
  `read_mb_s` — are added to that dictionary in the same change.
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
- **Raising `compute.max_concurrent`** stays out: more builds on one spindle
  multiply seeks, and above 1 the batch's bias/darkflat → dark → flat ordering
  is already documented as best-effort.
- A stray `PixInsight's` in a doc comment at `light_cal.rs:714` violates the
  repo rule against naming other codebases in code and comments. Fix it in
  passing when that file is touched.

## 9. Acceptance

Measured with the checked-in `examples/band_profile.rs` harness and the
eviction protocol from research §2, on the profiling machine.

| Gate | Before | After |
| ---- | ---- | ---- |
| 100-frame bias, cold, end to end | 241.6 s | **≤ 40 s** |
| 100-frame bias, read throughput | 22 MB/s | **≥ 150 MB/s** |
| 30-frame dark, cold, end to end | 11.8 s | ≤ 8 s |
| LDN 1272 batch (11 sets, ~23 GB) | 13.5 min | **≤ 5 min** |
| Master pixels | — | **byte-identical** to the pre-change build for the same inputs |
| SSD-backed scan root | not measured | open item — the class D3 expects to benefit most from concurrency |
| Network mount (SMB/NFS) | not measured | open item — the one class whose policy is reasoned, not measured |
| `cargo test --workspace` | green | green |
| `cargo check -p athenaeum-core --no-default-features` | green | green |
