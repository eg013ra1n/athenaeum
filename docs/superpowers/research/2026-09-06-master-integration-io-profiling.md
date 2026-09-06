# Master-integration I/O profiling — why a master build takes minutes

**Date:** 2026-09-06
**Trigger:** an 11-set "Create master" batch on the frame set LDN 1272 ran
13.5 min (07:06:48 → ~07:20 UTC) in the production desktop app with no visible
progress and no log line of its own.
**Question asked:** why so slow, when nothing saturates IOPS, CPU or RAM?
**Answer, in one line:** the banded reader pulls **22 MB/s off a drive that
sustains 243 MB/s** — the loss is entirely in our access pattern, not in the
hardware.

---

## 1. Machine and data under test

| | |
| ---- | ---- |
| Host | macOS 25.5, 10 cores, **16 GB RAM** |
| Source volume | `/Volumes/bigbase2` — `/dev/disk5s1`, APFS on **WDC WD8005FFBX**, external SATA, 7200 rpm, **not** an SSD |
| Library volume | `/Volumes/bigbase3` — `/dev/disk11s1`, APFS |
| Test set | 100 × BIAS, ZWO ASI2600MC Duo, 6248 × 4176, BITPIX 16 → **5.2 GB** |
| Second set | 30 × DARK 1.00 s, same camera → 1.56 GB |
| Batch actually observed | 11 calibration sets, ~440 frames, **~23 GB read** |

Recipe resolved by `api::masters::resolve_recipe` for every set of ≥ 15
frames: `average + WinsorizedSigma { 3.0, 3.0 }`.

## 2. Method

`crates/athenaeum-core/examples/band_profile.rs` drives the **production**
`BandSource` + `combine_pixel` with the production band loop copied verbatim
from `integration::engine::run_banded`, and times the read phase and the
combine phase separately. It is checked in (Task 1 of the plan) precisely so
every later change is measured the same way.

Page cache had to be defeated by hand — `purge(8)` returns
`Unable to purge disk buffers: Operation not permitted` for a non-root user,
and `F_NOCACHE` (fcntl 48) proved unreliable on APFS: a synthetic harness
using it reported 103 MB/s and 252 MB/s for the *same* configuration on two
consecutive runs. **The eviction protocol that does work** — and the one the
harness documents — is to stream more unrelated data than the machine has RAM
between runs:

```bash
find /Volumes/bigbase2/Astrobase/Calibration/dark6200 -name '*.fit*' \
  | head -400 | tr '\n' '\0' | xargs -0 -n4 cat | wc -c
```

That reads 25.7 GB in 105.8 s, which is itself the cleanest measurement of
what the drive can do: **243 MB/s** for plain whole-file sequential reads.
A single-file `dd` gives 181 MB/s. Both are far above anything the integration
engine achieves.

Runs marked *cold* below were each preceded by that eviction pass. Runs where
the 5.2 GB set still fit in the 16 GB page cache are marked *warm* and are used
only for the compute number, which is cache-independent.

## 3. Measurements

### 3.1 The 100-frame bias set, cold

| Configuration | bands | read | combine | total |
| ---- | ---- | ---- | ---- | ---- |
| **shipping today** — 256 MiB budget, serial reads | 40 | 235.5 s — **22 MB/s** | 6.0 s | **241.6 s** |
| 256 MiB budget, parallel `pread` | 40 | 216.0 s — 24 MB/s | 5.9 s | 221.9 s |
| 1 GiB budget, serial reads | 10 | 94.4 s — 55 MB/s | 6.8 s | 101.2 s |
| **4 GiB budget, parallel `pread`** | 3 | 37.1 s — **141 MB/s** | 7.3 s | **44.4 s** |
| *reference: whole-file sequential* | — | *21.5 s — 243 MB/s* | — | — |

### 3.2 The 30-frame dark set

Cold, shipping configuration (its 1.56 GB gives 335-row bands, 13 of them):

```
read              9.07s   173 MB/s
compute           2.70s
TOTAL            11.77s   read 77%  compute 23%
```

Warm (page-cached) runs of the same set, used to isolate the combine cost,
agree on **2.05–2.19 s** for 30 frames × 26 Mpx of winsorized sigma clipping
across 10 threads ≈ 370 M samples/s.

### 3.3 The model reproduces the complaint

23 GB at the 22–55 MB/s the shipping configuration achieves is 700–1050 s.
The observed batch took **810 s**. The model is not missing a hidden cost.

## 4. Root cause — three structural facts, all visible in the source

**F1 — the band budget is a hardcoded 256 MiB, sized in f32.**
`integration/engine.rs:14` fixes `BAND_BUDGET_BYTES = 256 * 1024 * 1024`, and
`banded.rs::band_rows_for_budget` divides it by `(frame_count + 2) * width * 4`
— f32 bytes, whatever the source depth is. For 100 frames of width 6248 that
is 105 rows per band ⇒ **40 passes over all 100 files ⇒ 4000 seeks**, each
read only 1.31 MB. The drive head crosses the platter a hundred times per
band and neither its own nor the kernel's read-ahead survives the
interleaving. This single constant is worth **5.3×** (241.6 s → 44.4 s).

**F2 — `read_band` is a serial `for` over the frames.**
`banded.rs:186` walks `self.readers.iter_mut()` doing `seek` + `read_exact`
one file at a time: queue depth 1, one thread, and the big-endian → f32 decode
loop runs inside that same serial phase. Worth only **+8 %** on this 7200 rpm
drive (seek-bound; NCQ cannot reorder its way out of 100 interleaved streams),
but it is the difference between one and ten outstanding requests on any SSD.

**F3 — read and combine never overlap.**
`engine.rs:96-106` is `src.read_band(...)` followed by `pool.install(...)`,
strictly alternating. This is the `top` signature the owner saw: **1.5 % CPU →
85 % → 1.5 %**, one core during I/O and ten during the combine, never both.

Secondary: `integrate_flat_inner` (`engine.rs:229-247`) reads the central third
of every flat for the per-frame means, then reads the whole frame again to
combine — **133 % of the data** — and its pass-1 inner loop is scalar.

## 5. Root inputs, questioned

Each candidate explanation was checked before the pattern was blamed:

- **Is the disk simply slow?** No. 243 MB/s whole-file sequential, 181 MB/s
  single-file `dd`. We reach 9 % of that.
- **Is it CPU?** No. The combine is 2.5 % of the cold 100-frame run (6.0 s of
  241.6 s) and it already uses all 10 cores via the shared `image_pool`.
- **Is it RAM?** No. 16 GB total, the process peaked at 493 MB. The budget is
  self-imposed, not forced.
- **Is it the ComputeQueue serializing the batch?** It does serialize
  (`compute.max_concurrent` default 1), but that is correct here — two
  concurrent builds would only multiply the seeking on one spindle. The
  per-set time is the problem, not the ordering.
- **Is it the writes?** No. 11 masters × 104 MB = 1.1 GB to a different volume.
- **Is it the DB / registration?** No. `start_master_builds_batch` closes in
  2.00 ms; the whole registration path is a single transaction per master.

## 6. External validation

The reference implementation most owners compare against exposes exactly this
lever to the user rather than hardcoding it: ImageIntegration has a per-file
**buffer size** and a total **stack size**, community guidance is that they be
"specified less than physical RAM", that the process "will break the image
into sets of rows and process each set as needed", and that leaving them small
means the integration "will just take longer to complete" — the documented
remedy for out-of-memory being to drop buffer size from 16 MB to ~1 MB and
stack size from 1024 MB to ~100 MB, explicitly trading speed for memory.

Two things follow, and both match what was measured here:

1. The industry-normal design is **two levers sized against physical RAM**, not
   one compile-time constant. Our 256 MiB is roughly a quarter of that
   implementation's *default* stack size, on a machine with 16 GB.
2. Nobody claims small chunks are free. The trade is understood to be speed
   against memory, which is precisely the trade our constant makes silently and
   permanently in favour of memory we are not short of.

Sources:
- [Buffer / Stack option — image integration (PixInsight Forum)](https://pixinsight.com/forum/index.php?threads/buffer-stack-option-image-integration.10735/)
- [Pixinsight buffer and stack size weirdness (Cloudy Nights)](https://www.cloudynights.com/forums/topic/614715-pixinsight-buffer-and-stack-size-weirdness/)
- [Where is the bottleneck with integration speed (Cloudy Nights)](https://www.cloudynights.com/topic/788344-where-is-the-bottleneck-with-integration-speed-on-pixinsight/page-2)

## 7. What the measurements rule *out* as a fix

- **Double-buffering / prefetching the next band.** At the 3-band
  configuration the combine is 7.3 s of 44.4 s (16 %). Affording a second set
  of band planes means halving the budget, which costs more than 16 % by the
  curve in §3.1. The measured cure for the read/compute alternation is *fewer,
  bigger bands* — not a second buffer. This is also what the owner asked for:
  no copies into memory or cache.
- **Any staging copy or transposed scratch file.** Reading each source once
  into a band-major scratch layout would reach 243 MB/s, but it writes and then
  re-reads the whole 23 GB. Rejected on the same instruction.
- **Raising `compute.max_concurrent`.** More builds on one spindle multiplies
  seeks; and `api::masters` documents that above 1 the batch's
  bias/darkflat → dark → flat dependency ordering degrades to best-effort.

## 7a. One storage class was measured, and there are three

Everything above was taken on a **single local 7200 rpm SATA drive**. That is
the owner's calibration storage today; it is not the only shape the app has to
serve, and the gap matters because one of the other two inverts a conclusion.

| Class | Measured here | What it implies |
| ---- | ---- | ---- |
| Local, rotating | yes — every number in §3 | seek-bound; band size dominates; 10-way concurrency measured **+8 %**, i.e. it did not hurt |
| Local, solid state | **no** | no seek penalty, so band size should matter less and concurrency more — queue depth 1 is the whole bottleneck on NVMe |
| Network (NFS/SMB) | **no** | latency-bound, not seek-bound: throughput is set by how many requests are outstanding, and filling a link needs **more** of them than the machine has cores |

The network row is the one that changes the design rather than a constant. If
read concurrency rides the CPU thread pool — which was the shape of the first
draft of the plan — it is capped at `available_parallelism()`, and a NAS mount
simply cannot be filled. Reads therefore have to take their parallelism as a
parameter rather than inherit a pool, and that is a structural decision, not
something to retrofit after a measurement.

The rotational/solid split, by contrast, has no evidence behind it in either
direction: the spinning disk we do have got *faster* with ten readers, not
slower. Distinguishing the two would also mean asking IOKit on macOS whether a
volume rotates — the one probe of the three that is genuinely awkward — for a
verdict nothing measured would act on.

Hence the design's two classes, `Local` and `Network`, detected from a
deterministic OS property (`MNT_LOCAL` on macOS, the `statfs` filesystem magic
on Linux, `GetDriveTypeW`/UNC on Windows) rather than by timing anything. The
SSD and network numbers stay named, open and cheap to take once the code is in
— and until the network one exists, its policy is labelled in the spec as
reasoned rather than measured, with a setting as the escape hatch.

## 7b. The network measurement was attempted and did NOT produce a number

The prod catalog has a network scan root — id 7, `/Volumes/Universe/Astrophotography`,
an SMB 3.1.1 share on `AstroDB` over 2.5GBase-T (312 MB/s link ceiling). Three
attempts were made to measure read concurrency against it on 2026-09-06. **All
three were invalid, and no network throughput number exists.** They are recorded
here so the next attempt does not repeat them.

**What DID come out of it, and it is worth having:** the storage-class probe the
design relies on was validated against real hardware, exactly as specified —

| Path | `f_fstypename` | `MNT_LOCAL` | Verdict |
| ---- | ---- | ---- | ---- |
| `/Volumes/Universe/Astrophotography` (scan root 7) | `smbfs` | clear | **Network** |
| `/Volumes/bigbase2/Astrobase` | `apfs` | set | Local |
| `/Volumes/bigbase3/Calibration` | `apfs` | set | Local |

### Why each attempt failed

1. **Evict-between-runs, 9 GB of local reads.** Repeating the *identical* first
   configuration as the last run gave **275.1 MB/s** against its own
   **163.6 MB/s** — a 1.7x spread on unchanged settings, larger than the spread
   between the configurations being compared (228-294 MB/s). 9 GB does not
   evict a 1 GB working set from a 16 GB page cache reliably; the §2 protocol's
   full 25.7 GB does.
2. **Warm-everything-then-interleave.** Reported 13 974-24 844 MB/s. That is
   memory bandwidth: after the warm-up the whole 1 GB set lived in the client
   page cache and every `pread` was a memcpy. A 2.5 Gbps link cannot exceed
   312 MB/s, which is why the harness now refuses to print a table whose
   medians clear the link ceiling.
3. **`F_NOCACHE` to bypass the client cache.** Every `pread` returned
   `EIO`. The obvious reading — smbfs does not honour `F_NOCACHE` — is **not
   established**: by the time the failure was investigated the share had
   **unmounted**, while the NAS itself stayed reachable (0.5 ms ping). An EIO
   storm is what a mount disappearing under an open fd looks like, so the
   likelier cause is the disconnect, not the fcntl.

### The unmount, stated without overclaiming

The share was mounted when the sweep started and gone when it ended. macOS
`log show` yields no smb events at all for the window, so the cause is **not
established**. The 32-reader configuration is a candidate; an unrelated NAS or
Wi-Fi/Ethernet event is an equally good one. It is one occurrence with no
mechanism proven.

It is recorded because *if* heavy concurrency can drop an SMB session, the
`Network` ceiling stops being only about throughput and becomes about not
destabilising the client — which would make the ceiling a correctness bound
rather than a tuning one. That is a hypothesis, not a finding.

### The protocol the next attempt should use

- Working set: as large as the share can offer. The one used here — 19 frames,
  ~1.0 GB, `AstroTMP/Tests/ZWO 2600mm tests/SNAPSHOT` — is the biggest single
  FITS directory on the share (all of `/Astrophotography` holds 16 files; the
  11 TB is jpg/ser/json and empty directory skeletons).
- Evict with the **full** §2 pass (25.7 GB local, ~106 s) between every run.
  That clears the CLIENT cache; the NAS's own cache cannot be evicted from
  here, so the condition is cold-client/warm-server. Say so: a cold server adds
  per-request latency, which can only make concurrency matter more, so any
  number obtained this way is a **lower bound** on its value.
- Repeat the first configuration last as a validity gate, and discard the run
  if it does not reproduce within a few percent. That gate is what caught
  attempt 1.
- Keep the link-ceiling assertion. That gate is what caught attempt 2.
- Climb the concurrency ladder rather than jumping to 32, and stop on the first
  I/O error instead of continuing — attempt 3 kept issuing reads into a dead
  mount.
- **Band size cannot be measured on this share at all.** Its effect is about
  seeks on the server's disk, and a warm server never reaches its disk. The
  band-size lever stays measured on local storage only (§3.1, 5.3x).

## 8. Observability gap found on the way

Separate from throughput, and the reason the owner could not tell what the app
was doing: **a master build emits no `info`-level log at all.** `api/masters.rs`
carries only `warn!` (non-finite samples) and `error!` (failures) on the build
path; between `start_master_builds_batch` closing at 07:06:48 and the next
unrelated line the log is empty for the entire 13.5 minutes. The frontend has
the data and discards it — `master-build-progress` carries `stage`, `current`,
`total`, `percent`, and `CalibrationHierarchyView.tsx:129-133` reduces the whole
event to `s.phase` (`'starting' | 'building' | 'done'`); `ComputeQueueIndicator`
shows a label and `running`/`queued` with no fraction.
