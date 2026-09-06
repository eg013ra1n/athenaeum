# Full-resolution VNG debayer in the Blink viewer

**Date:** 2026-09-06
**Status:** approved — ready to implement
**Scope:** rustafits pipeline, `athenaeum-core` render + preview cache, desktop IPC, Settings UI

## 1. Problem

`Resolution::Full` lies for a one-shot-colour frame.

Every CFA frame — at all three resolutions — goes through
`super_pixel_debayer_u16` / `super_pixel_debayer_f32`
(`rustafits/src/pipeline.rs`), which folds each 2×2 Bayer tile into a single
output pixel. A 6248×4176 OSC light therefore renders at 3124×2088 even when the
user explicitly asks for full resolution with the Blink toolbar's ScanEye button
(`src/components/blink/ToolBar.tsx` → `read_fits_image_rustafits` /
`get_frame_preview` with `resolution: "full"`). Half the sampled detail is thrown
away before the image reaches the canvas, so 1:1 star inspection — the reason the
button exists — cannot be done on OSC data at all.

rustafits already ships the replacement: `processing::vng::vng_debayer_f32`, the
eight-gradient VNG demosaic written for the calibrated-lights export. It keeps
the native pixel grid and emits planar RGB, and it is already validated against
an external reference implementation (`vng_matches_reference_debayer`).

## 2. Measurements

All numbers below were measured, not estimated, with a throwaway probe that calls
the same public rustafits functions the pipeline calls — `ImageConverter::read_raw`
→ `color::u16_to_f32` → `vng::vng_debayer_f32` → per-channel `stretch::apply_stretch`
→ `encode_jpeg` — on a real frame:
`Light_Gamma Scuti_180.0s_Bin1_20251018-195122_0003.fit`, ZWO ASI2600MC Pro,
6248×4176, RGGB, 52 MB. macOS, 10 cores, machine otherwise idle.

| | superpixel (today) | VNG (proposed) |
| ---- | ---- | ---- |
| output | 3124×2088 | 6248×4176 |
| debayer | 6 ms | **633 ms** |
| stretch | 37 ms | 59 ms |
| JPEG encode (q95) | 23 ms | 85 ms |
| total | 89 ms | **793 ms** |
| JPEG size (q95) | 4.5 MB | **17.2 MB** |
| peak RSS | 218 MB | **547 MB** |

JPEG quality is a real lever and is already a user setting
(`rustafits.quality.full`, default 95): q95 → 17.2 MB, q90 → 12.1 MB,
q85 → 9.6 MB, q80 → 8.0 MB. Encode time barely moves across that range.

**Concurrency buys nothing.** Five frames rendered in parallel: 3.40 s wall.
The same five sequentially: 3.99 s. A 15 % gain — and a generous one, because the
probe ran five separate processes each owning a full rayon pool, while the app
shares one `image_pool` across all renders (`rustafits/src/converter.rs` wraps
every call in `pool.install`). VNG already saturates the machine on its own:
5.84 s of user time inside 0.79 s of wall time, i.e. ~7.4 of 10 cores. What
parallelism does buy is a 5× larger memory peak: 2.7 GB instead of 547 MB.

## 3. Decisions

- **D1 — `Resolution::Full` on a CFA frame means native VNG, everywhere.**
  Both entry points get it: the ScanEye button (which already sends
  `resolution: "full"`) and the `blink.resolution` setting, on which the whole-set
  prefetch runs. Settings must say out loud that this slows buffering.
  *(Owner-ratified.)*
- **D2 — full-resolution mode still resets on frame navigation.** Unchanged
  behaviour (`BlinkViewer.tsx:420`); pressing → returns to preview and the button
  must be pressed again. *(Owner-ratified.)*
- **D3 — the default of `rustafits.quality.full` stays 95.** The number is now
  known and the control already exists; we do not silently change what an
  existing setting produces.
- **D4 — one VNG render at a time, process-wide.** See §4.2. Costs ~15 % of
  wall-clock on a burst, saves ~2.2 GB of peak RAM.
- **D5 — the preview cache gets a byte budget.** See §4.3. Without it, D1 turns a
  latent problem into a live one: at 200 entries × 17.2 MB the JPEG cache alone
  holds 3.4 GB for 30 minutes.

## 4. Design

### 4.1 rustafits — opt-in VNG in the pipeline

**Files:** `rustafits/src/types.rs`, `rustafits/src/converter.rs`,
`rustafits/src/pipeline.rs`.

`ProcessConfig` gains `vng_debayer: bool` (default `false`). `ImageConverter`
gains the matching field and a `with_vng_debayer()` builder, wired through
`build_config`.

In `pipeline::process_u16`, the CFA branch (`config.apply_debayer &&
meta.bayer_pattern != BayerPattern::None`) forks on the new flag:

- **off** — unchanged: `super_pixel_debayer_u16`, half dimensions, and the
  existing `extra = config.downscale_factor / 2` compensation because the
  debayer itself already counted as a 2× reduction.
- **on** — `color::u16_to_f32(&data)`, drop the u16 buffer, then
  `vng::vng_debayer_f32(&f, width, height, meta.bayer_pattern)`. Dimensions stay
  native, so `config.downscale_factor` applies **in full** — the `/2`
  compensation must not be carried over. `is_color = true`, `num_channels = 3`,
  and the result goes through the unchanged `apply_stretch_and_finalize`.

`process_f32` forks the same way, minus the conversion.

Two properties make this safe without extra guards:

- **Scale is identical.** `debayer_u16_row` and `u16_to_f32` both write raw ADU
  as `f32`, so the per-channel `compute_stretch_coefficients` sees the same
  numbers it sees today. Colour will not shift; the image is simply sharper and
  four times the pixels.
- **Small images are already handled.** `vng_debayer_f32` derives its interior
  range with `saturating_sub`, so a frame narrower or shorter than the 5×5 window
  yields an empty interior loop and is fully covered by `bilinear_border`.

**Tests** (`rustafits/src/pipeline.rs`, using `ImageConverter::process_data` so no
file I/O is needed):

- `vng_keeps_native_dimensions` — 16×16 RGGB `PixelData::Uint16` with the flag on
  produces a 16×16, 3-channel, `is_color` result.
- `superpixel_still_halves_without_the_flag` — the same input with the flag off
  produces 8×8. Pins that the default path is untouched.
- `vng_applies_the_full_downscale_factor` — flag on + `with_downscale(2)` gives
  8×8, proving the `/2` compensation was dropped rather than inherited.
- `vng_covers_the_f32_input_path` — same as the first, via `PixelData::Float32`.

**Acceptance:** `cargo test -p rustafits` green; no change to any output when
`vng_debayer` is false.

### 4.2 core — `Full` ⇒ VNG, and one VNG render at a time

**File:** `crates/athenaeum-core/src/rustafits_processor/mod.rs`.

`process_fits_to_jpeg` adds `.with_vng_debayer()` to the converter when
`resolution == Resolution::Full`. `Preview` and `Thumbnail` are untouched. Because
both hosts funnel through this one function — desktop via
`commands_rustafits::read_fits_image_rustafits` (`block_in_place`), web via
`routes/images.rs::get_frame_preview` (`spawn_blocking`) — the two backends stay
in sync with no new command, route, or serde type.

The function also stops calling `converter.process(path)` and instead splits the
read from the processing, so the gate can be taken only when VNG actually
engages:

```rust
let (meta, pixels) = pool.install(|| ImageConverter::read_raw(input_path))?;
let vng_path = resolution == Resolution::Full
    && meta.channels == 1
    && meta.bayer_pattern != BayerPattern::None;
let _gate = vng_path.then(|| VNG_GATE.lock().unwrap_or_else(|e| e.into_inner()));
let processed = converter.process_data(meta, pixels)?;
```

**The `pool.install` around `read_raw` is load-bearing.** `ImageConverter::read_raw`
is an associated function with no `&self`; unlike `process` / `process_data` it
does **not** install the converter's thread pool, and `formats::fits::read_fits_image`
parallelises its byte-swap with rayon (`rustafits/src/formats/fits.rs:187` and
friends). Calling it bare would quietly move that work from the app's bounded
`image_pool` onto the global rayon pool. Nesting `install` on the same pool is
fine, so the read and the process each get their own.

`VNG_GATE` is a `static std::sync::Mutex<()>` in this module. A plain std mutex is
the right primitive here: both hosts call this function from a blocking context
(`block_in_place` / `spawn_blocking`), so blocking on it is sanctioned, and
poisoning is recovered with `into_inner` so one panicking render cannot disable
previews for the rest of the session. Waiters hold only the raw pixel buffer
(2 B/px ≈ 52 MB for the reference sensor), not the 12 B/px VNG output.

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

**Test** (`crates/athenaeum-core/src/rustafits_processor/mod.rs`): write a small
f32 CFA FITS with `write_fits_f32` and a `BAYERPAT = 'RGGB'` card, then assert
`process_fits_to_jpeg(.., Resolution::Full, ..)` returns native dimensions while
`Resolution::Preview` returns halved ones. This is the end-to-end pin for the
wiring, in the crate that owns it.

**Acceptance:** an OSC frame at `Full` comes back at native size from both hosts;
a mono frame at `Full` is byte-identical to today and never takes the gate.

### 4.3 core — the preview cache gets a byte budget

**File:** `crates/athenaeum-core/src/cache/memory.rs` (+ four wiring sites).

`MemoryImageCache` counts **entries**, not bytes: default 200, user-settable
10…5000 in Settings, held for `blink.memory_retention_minutes` (default 30). The
doc comment — "At ~300KB per JPEG, 200 entries ≈ 60MB" — is written for preview
JPEGs. At 17.2 MB per full-resolution VNG frame the same 200 entries are 3.4 GB.
This is already wrong at `Full` today (200 × 4.5 MB = 900 MB); D1 makes `Full`
worth choosing, which is what turns a sleeping bug into a live one.

The cache gains `max_bytes` and a running `current_bytes`:

- `new(max_entries, retention_minutes)` **keeps its signature** — it is called
  from ~30 sites, nearly all of them test/stub `ServiceContext`s — and defaults
  `max_bytes` to 512 MB.
- `with_max_bytes(bytes)` (builder) is used at the two real construction sites;
  `set_max_bytes(bytes)` mirrors `set_max_entries` for live settings updates.
- `insert` adds the new entry's `data.len()`, subtracts a replaced entry's, then
  evicts from the front while `entries.len() > max_entries || current_bytes >
  max_bytes` — **stopping at one entry**, so a single frame larger than the whole
  budget is still served rather than evicting itself and turning every request
  into a miss.
- `evict_stale`, `set_max_entries`, `set_max_bytes` and `clear` all maintain
  `current_bytes`.

New setting `blink.memory_cache_max_mb`, default `512`, accepted range
64…16384 — 512 MB is ~30 full-resolution OSC frames, while a preview-sized cache
(~300 KB/frame) stays bound by the entry limit as it is today, so the budget only
bites in exactly the case it exists for.

**Wiring:** `crates/athenaeum-tauri/src/lib.rs` (initial construction + the
DB-driven update alongside `set_max_entries`/`set_retention`),
`crates/athenaeum-tauri/src/commands/settings.rs`,
`crates/athenaeum-web/src/main.rs`,
`crates/athenaeum-web/src/routes/settings.rs`. Both backends change together.

**Tests** (`cache/memory.rs`):

- `byte_budget_evicts_before_the_entry_limit`
- `an_entry_larger_than_the_budget_is_still_kept`
- `replacing_a_key_updates_the_byte_total`
- `evict_stale_updates_the_byte_total`
- `set_max_bytes_evicts_immediately`

**Acceptance:** blinking a 300-frame OSC set at `Full` holds ≤ 512 MB of JPEG in
the cache instead of growing to 3.4 GB.

### 4.4 desktop — the JPEG travels as bytes, not as a JSON array

**Files:** `crates/athenaeum-tauri/src/commands_rustafits.rs`,
`crates/athenaeum-tauri/src/commands/files.rs`, `src/components/BlinkViewer.tsx`.

`read_fits_image_rustafits` returns a plain `Result<Vec<u8>, String>`. Verified in
the pinned source (`tauri-2.11.5/src/ipc/mod.rs:181`): the blanket
`impl<T: Serialize> IpcResponse for T` runs `serde_json::to_string(&self)` and
yields `InvokeResponseBody::Json` — a 17-million-element JSON array of numbers for
one full-resolution VNG frame. `tauri::ipc::Response::new(bytes)` instead yields
`InvokeResponseBody::Raw`, which `ipc/protocol.rs:346` sends as
`application/octet-stream` and the injected JS reads with `response.arrayBuffer()`
(`tauri-2.11.5/scripts/ipc-protocol.js:52`). `tauri::ipc::Response` is used nowhere
in this repo today.

Change: extract the current body into `async fn read_fits_image_bytes(...) ->
Result<Vec<u8>, String>`; the `#[tauri::command]` wraps it in
`tauri::ipc::Response::new(...)`, and `commands/files.rs::get_frame_preview` calls
the byte-returning function directly (it base64-encodes for a data URI and needs
`Vec<u8>`). The web route already returns raw bytes with a `Content-Type` header
and needs no change.

Frontend: both `api.invoke` call sites in `BlinkViewer.tsx` already fall back to
`new Uint8Array(imageData as number[])`, which happens to accept an `ArrayBuffer`
correctly. Make that explicit rather than accidental — handle `Uint8Array`,
`ArrayBuffer`, and the legacy `number[]` — so the web build (raw bytes) and the
desktop build (ArrayBuffer) both read clearly.

**Acceptance:** desktop full-resolution load of an OSC frame is limited by the
793 ms render, not by JSON parsing. Measure it in the running app; if the
transfer still dominates, that finding goes to `docs/open-items.md` rather than
being patched blind.

### 4.5 frontend — say what Full costs

**Files:** `src/pages/Settings.tsx`, `src/components/blink/ToolBar.tsx`.

- Under the `blink.resolution` select: a note that Full Resolution debayers
  one-shot-colour frames at native resolution and noticeably slows whole-set
  buffering. Design tokens only (`text-content-muted`), no raw colours.
- Next to the new memory-cache-limit field: what it is for, in one line.
- ScanEye tooltip: name what Full actually delivers now.

## 5. Non-goals

- No change to `Preview` / `Thumbnail`. Blink playback stays fast.
- No new resolution value, no third button state, no sticky full-res mode (D2).
- No white balance, no change to the stretch. Per-channel auto-stretch already
  neutralises the raw OSC cast, and VNG output feeds the identical code.
- No downscale-after-VNG compromise. If the IPC measurement in §4.4 says one is
  needed, that is a separate decision with a number attached.
- The webview-side blob footprint (`loadedImages` holds one blob URL per cached
  frame, the same order of magnitude as the Rust cache) is **not** addressed here.
  It is recorded in §7 as a known consequence.

## 6. Testing and gates

- `cargo test -p rustafits` — §4.1 pipeline tests.
- `cargo test --workspace` — §4.2 and §4.3 tests; the workspace, not just core,
  because the web route tests are where a core-only run goes blind.
- `cargo check -p athenaeum-core --no-default-features` — the headless gate. The
  render path is behind the `render` feature; this is where a missing `#[cfg]`
  shows up (it is exactly what broke the first v0.5.5 tag).
- `npx tsc --noEmit`.
- **Real data**, not synthetic: render
  `~/Pictures/Astro/Unsorted/pigmey/Gamma Scuti/Light_Gamma Scuti_180.0s_*.fit`
  at `Full` in the running desktop app and confirm the canvas is 6248×4176,
  colour is unchanged versus today's half-size render, and the star overlay still
  lands on stars (`StarOverlay` scales by the render/analysis ratio, so a
  resolution change should be transparent — this verifies it).

## 7. Risks and accepted consequences

- **Full-set buffering at `Full` gets ~9× slower** (89 ms → 793 ms per frame,
  serialised by the gate). Accepted and stated in the UI (D1).
- **The gate costs ~15 % on a burst.** Measured. It buys a 5× smaller memory peak.
- **The webview holds one blob per cached frame.** At `Full` those blobs are
  17.2 MB each, mirroring the Rust cache on the renderer side. The Rust-side byte
  budget does not bound it. Out of scope here; if it bites, the fix is to bound
  `loadedImages` the same way.
- **`Full` on a very large sensor is heavier still.** 61 Mpx (e.g. ASI6200MC)
  scales the 547 MB peak to roughly 1.3 GB per render. The gate keeps that a peak
  of one, not five.
- **Imported RGB (3-channel) files are unaffected** — they never enter the CFA
  branch.

## 8. Release-note lines owed

- Full resolution in Blink now debayers one-shot-colour frames at their native
  resolution with gradient-based interpolation, instead of halving them.
- The Blink image cache now has a memory limit in megabytes, not just a frame
  count.
