//! FITS/XISF → JPEG preview rendering for the library browser (feature `preview`).
//!
//! A capture node's operator wants to *look* at a frame before deciding to send
//! or delete it. This module is the whole backend for that: one file in, one
//! auto-stretched JPEG out, with the two guards a Raspberry Pi needs.
//!
//! # The two guards
//!
//! 1. **`Semaphore(1)`** — at most one render is in flight per process. A
//!    full-frame stretch is the single heaviest thing Perseus ever does (a
//!    6248×4176 frame is ~50 MB of `f32` plus the RGB output); letting a browser
//!    that just painted a grid of thumbnails start eight of them at once would
//!    put a 2 GB Pi into swap. Requests queue instead.
//! 2. **An 8-entry LRU** — re-requesting the frame you are already looking at
//!    (a resize, a re-render after a tab switch, the 200-then-304 revalidation
//!    round trip) must not re-decode it. Bounded by *count*, not bytes: at the
//!    [`MAX_WIDTH`] cap a JPEG is a few hundred KB, so eight of them is a
//!    couple of MB worst case.
//!
//! # Cache identity and the ETag
//!
//! An entry is keyed on `(canonical path, size, mtime_ms, clamped width)` —
//! [`PreviewKey`]. Keying on the **canonical absolute path** rather than the
//! `(root_index, rel_path)` the wire uses is deliberate: [`resolve_in_root`]
//! canonicalizes anyway, so the two are the same identity, and two capture roots
//! that alias one directory then correctly share a single entry instead of
//! rendering it twice. `size` + `mtime_ms` are what make a *rewritten* file
//! (the capture software re-saving over the same name) miss the cache.
//!
//! [`PreviewKey::etag`] is a hash of exactly that tuple, which is what makes the
//! HTTP contract cheap: a conditional request can be answered `304` from the
//! stat alone — no semaphore, no render, no cached bytes needed.
//!
//! [`resolve_in_root`]: crate::library::resolve_in_root

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use tokio::sync::Semaphore;

/// Widest preview this node will render. A preview is a *look*, not a master:
/// beyond this the JPEG costs more to ship than the operator gains, and the
/// downscale is what keeps the encode cheap on a Pi.
pub const MAX_WIDTH: u32 = 1600;

/// Narrowest preview this node will render. Floors nonsense (`w=0`, `w=3`) into
/// something that still encodes and still shows the operator whether the frame
/// is trailed or fogged.
pub const MIN_WIDTH: u32 = 64;

/// JPEG quality for previews. 85 is the same "preview" quality the desktop app
/// uses — visually clean, roughly a third the bytes of the 95 it keeps for
/// full-resolution renders.
const JPEG_QUALITY: u8 = 85;

/// Rendered previews kept in memory. See the module docs for why this is a
/// count and not a byte budget.
const CACHE_CAP: usize = 8;

/// Clamp a requested width into the renderable range.
///
/// Applied **once**, inside [`PreviewKey::stat`], so the cache key and the
/// rendered pixels can never disagree: `?w=9999` and `?w=1600` are the same
/// request, share one cache entry, and carry the same ETag.
pub fn clamp_width(w: u32) -> u32 {
    w.clamp(MIN_WIDTH, MAX_WIDTH)
}

/// The identity of one rendered preview: which bytes on disk, at what width.
///
/// Built by [`stat`](Self::stat) so that a single `stat(2)` serves both the
/// conditional-request check and the cache lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewKey {
    /// Canonical absolute path, lossily stringified. Lossy is safe here: this
    /// value is only ever hashed and compared, never resolved back to a path.
    id: String,
    size: u64,
    mtime_ms: i64,
    width: u32,
}

impl PreviewKey {
    /// Stat `abs` and build the key for rendering it at `w` (clamped).
    ///
    /// `abs` is expected to be the already-guarded, already-canonical path from
    /// [`resolve_in_root`](crate::library::resolve_in_root). A failure here is a
    /// TOCTOU loss (the file went away between the resolve and the stat) and is
    /// reported with the same `"not found"` prefix the library contract uses, so
    /// it maps to the same `404`.
    pub fn stat(abs: &Path, w: u32) -> Result<Self> {
        let meta =
            std::fs::metadata(abs).with_context(|| format!("not found: {}", abs.display()))?;
        // A pre-epoch or unreadable mtime degrades to 0 rather than failing:
        // `size` still discriminates a rewrite, and a preview is not worth a
        // 500 over an exotic timestamp.
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Ok(Self {
            id: abs.to_string_lossy().into_owned(),
            size: meta.len(),
            mtime_ms,
            width: clamp_width(w),
        })
    }

    /// The clamped width this key renders at.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// A strong ETag (quoted, per RFC 9110) over the whole key tuple.
    ///
    /// FNV-1a rather than `DefaultHasher`: the latter is explicitly not stable
    /// across Rust releases, so a toolchain bump would silently invalidate every
    /// browser's cached preview. This one is fixed forever. It is not a security
    /// primitive — a collision costs a stale preview, not an escape — but the
    /// path is length-delimited from the fixed-width tail so `("ab", 1, …)` and
    /// `("a", …)` cannot alias.
    pub fn etag(&self) -> String {
        let mut h = FNV_OFFSET;
        fnv1a(&mut h, self.id.as_bytes());
        fnv1a(&mut h, &[0xff]); // delimiter: ends the variable-length field
        fnv1a(&mut h, &self.size.to_le_bytes());
        fnv1a(&mut h, &self.mtime_ms.to_le_bytes());
        fnv1a(&mut h, &self.width.to_le_bytes());
        format!("\"{h:016x}\"")
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(FNV_PRIME);
    }
}

/// The process-wide preview renderer: the concurrency gate plus the LRU.
///
/// Lives on [`WebState`](crate::web::WebState), so it is created once at
/// supervisor start and shared by every request. Cheap to construct, never
/// rebuilt.
pub struct PreviewCache {
    /// One permit: exactly one render at a time (module docs, guard 1).
    ///
    /// Behind an `Arc` so the permit can be `acquire_owned`'d and moved INTO the
    /// blocking render — see [`render_jpeg`] for why an abandoned request must
    /// not hand its permit on early.
    gate: Arc<Semaphore>,
    /// Most-recently-used at the BACK. A plain `VecDeque` beats a real LRU map
    /// at this size — eight linear comparisons of a small key is nothing next to
    /// the render it is protecting.
    ///
    /// `Arc`ed for the same reason as `gate`: the blocking render inserts its own
    /// result, so the insert lands *before* the permit is released.
    entries: Arc<Lru>,
    /// How many real renders have run. An observability counter first (it is the
    /// honest answer to "is the cache doing anything?"), and the hook the cache
    /// tests assert on — an `Arc` because the blocking closure that does the
    /// work is moved onto another thread.
    renders: Arc<AtomicUsize>,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The LRU itself, split out so the blocking render can hold a handle to it
/// independently of the `&PreviewCache` borrow the caller has.
struct Lru(Mutex<VecDeque<(PreviewKey, Arc<Vec<u8>>)>>);

impl Lru {
    fn new() -> Self {
        Self(Mutex::new(VecDeque::with_capacity(CACHE_CAP)))
    }

    fn len(&self) -> usize {
        self.0.lock().expect("preview cache mutex").len()
    }

    /// Look `key` up, promoting a hit to most-recently-used.
    fn get(&self, key: &PreviewKey) -> Option<Arc<Vec<u8>>> {
        let mut entries = self.0.lock().expect("preview cache mutex");
        let idx = entries.iter().position(|(k, _)| k == key)?;
        let hit = entries.remove(idx)?;
        let bytes = Arc::clone(&hit.1);
        entries.push_back(hit);
        Some(bytes)
    }

    /// Insert (or refresh) `key`, evicting from the front until the cap holds.
    fn put(&self, key: PreviewKey, bytes: Arc<Vec<u8>>) {
        let mut entries = self.0.lock().expect("preview cache mutex");
        if let Some(idx) = entries.iter().position(|(k, _)| *k == key) {
            entries.remove(idx);
        }
        entries.push_back((key, bytes));
        while entries.len() > CACHE_CAP {
            entries.pop_front();
        }
    }
}

impl PreviewCache {
    pub fn new() -> Self {
        Self {
            gate: Arc::new(Semaphore::new(1)),
            entries: Arc::new(Lru::new()),
            renders: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Renders performed since start — cache misses that actually reached the
    /// decoder. A failed render counts (the work was done and thrown away).
    pub fn render_count(&self) -> usize {
        self.renders.load(Ordering::Relaxed)
    }

    /// Previews currently held. Never exceeds [`CACHE_CAP`].
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Render `abs` to JPEG bytes at `key`'s width, serving the cache when it can.
///
/// `key` must have been built from `abs` (via [`PreviewKey::stat`]) — it carries
/// both the cache identity and the width, so the caller's single stat covers the
/// conditional-request check and this call.
///
/// The cache is consulted **twice**: once before queueing on the gate, and again
/// after acquiring it. The second check is what makes N browser tabs opening the
/// same frame cost one render instead of N — without it every waiter would wake
/// up and redundantly re-render what the winner just cached.
///
/// Both the permit AND the cache insert live **inside the blocking task**, and
/// in that order — the entry is published before the permit is released. Two
/// things fall out of that, and both are load-bearing:
///
/// - An `<img>` whose `src` changes makes the browser abort the request, which
///   drops this future — but `spawn_blocking` work is detached and runs to
///   completion regardless. Holding the permit out here would release it at the
///   abort and admit the next render *alongside* the orphan, so a user clicking
///   quickly through a folder could put several full-frame stretches on the
///   blocking pool at once. Owning it inside means the orphan still counts, and
///   its result still lands in the cache instead of being thrown away.
/// - Inserting after the permit dropped left a window where the next waiter woke
///   up, re-checked an *empty* cache, and rendered the very same frame again.
///   Publishing under the permit closes it: whoever is admitted next is
///   guaranteed to see the finished entry.
pub async fn render_jpeg(
    cache: &PreviewCache,
    key: &PreviewKey,
    abs: &Path,
) -> Result<Arc<Vec<u8>>> {
    if let Some(hit) = cache.entries.get(key) {
        tracing::debug!(path = %abs.display(), width = key.width, "preview cache hit");
        return Ok(hit);
    }
    // `acquire_owned` only errors on a closed semaphore, and nothing ever closes
    // this one (it lives as long as the process).
    let permit = Arc::clone(&cache.gate)
        .acquire_owned()
        .await
        .context("preview semaphore closed")?;
    if let Some(hit) = cache.entries.get(key) {
        tracing::debug!(path = %abs.display(), width = key.width, "preview cache hit after gate");
        return Ok(hit);
    }

    let path: PathBuf = abs.to_path_buf();
    let width = key.width;
    let renders = Arc::clone(&cache.renders);
    let entries = Arc::clone(&cache.entries);
    let owned_key = key.clone();
    let started = std::time::Instant::now();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Arc<Vec<u8>>> {
        // Dropped at the end of this closure — after the insert below.
        let _permit = permit;
        renders.fetch_add(1, Ordering::Relaxed);
        let bytes = Arc::new(render_blocking(&path, width)?);
        entries.put(owned_key, Arc::clone(&bytes));
        Ok(bytes)
    })
    .await
    .context("preview render task panicked")??;

    tracing::info!(
        path = %abs.display(),
        width,
        count = bytes.len(),
        duration_ms = started.elapsed().as_millis() as u64,
        "preview rendered"
    );
    Ok(bytes)
}

/// The blocking half: decode + auto-stretch + downscale + JPEG encode.
///
/// `with_preview_mode` is rustafits's cheap path — 2×2 binning for mono frames,
/// which is the overwhelming majority of what a capture node holds — so the
/// expensive stretch runs over a quarter of the pixels. Whatever comes out is
/// then box-averaged down to the requested width; rustafits's own downscale
/// takes an integer factor, and hitting an exact target width matters more here
/// than saving one pass over an already-binned buffer.
fn render_blocking(abs: &Path, width: u32) -> Result<Vec<u8>> {
    let img = astroimage::ImageConverter::new()
        .with_preview_mode()
        .process(abs)
        .with_context(|| format!("render {}", abs.display()))?;
    if img.width == 0 || img.height == 0 {
        anyhow::bail!("render produced an empty image: {}", abs.display());
    }
    let channels = usize::from(img.channels);
    let (data, w, h) = fit_to_width(&img.data, img.width, img.height, channels, width as usize);
    astroimage::encode_jpeg(&data, w, h, channels, JPEG_QUALITY)
        .with_context(|| format!("encode preview jpeg for {}", abs.display()))
}

/// Downscale an interleaved 8-bit buffer to `target` width, preserving aspect.
///
/// Never upscales: a frame already narrower than the request is returned
/// borrowed and unchanged. Blowing a 1024-wide sub up to 1600 would cost bytes
/// and time to add exactly zero information.
fn fit_to_width<'a>(
    src: &'a [u8],
    sw: usize,
    sh: usize,
    ch: usize,
    target: usize,
) -> (Cow<'a, [u8]>, usize, usize) {
    if target >= sw || sw == 0 || sh == 0 {
        return (Cow::Borrowed(src), sw, sh);
    }
    // Integer aspect preservation; a very wide, very short frame still gets at
    // least one row.
    let th = ((sh * target) / sw).max(1);
    (
        Cow::Owned(box_downscale(src, sw, sh, ch, target, th)),
        target,
        th,
    )
}

/// Box-average `src` (`sw`×`sh`, `ch` interleaved channels) into `tw`×`th`.
///
/// Averaging rather than nearest-neighbour on purpose: a preview's whole job is
/// to show whether the stars are round, and point-sampling a star field at 1/8
/// scale drops most of the stars entirely.
fn box_downscale(src: &[u8], sw: usize, sh: usize, ch: usize, tw: usize, th: usize) -> Vec<u8> {
    debug_assert!(
        src.len() >= sw * sh * ch,
        "source buffer shorter than its geometry"
    );
    debug_assert!(ch <= 4, "at most RGBA");
    let mut out = vec![0u8; tw * th * ch];
    for dy in 0..th {
        let y0 = dy * sh / th;
        let y1 = (((dy + 1) * sh) / th).max(y0 + 1).min(sh);
        for dx in 0..tw {
            let x0 = dx * sw / tw;
            let x1 = (((dx + 1) * sw) / tw).max(x0 + 1).min(sw);
            // Accumulate all channels in one pass over the source box — looping
            // the box once per channel would triple the memory traffic.
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let row = y * sw * ch;
                for x in x0..x1 {
                    let p = row + x * ch;
                    for (c, a) in acc.iter_mut().enumerate().take(ch) {
                        *a += u32::from(src[p + c]);
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            let dst = (dy * tw + dx) * ch;
            for (c, a) in acc.iter().enumerate().take(ch) {
                out[dst + c] = (a / n) as u8;
            }
        }
    }
    out
}

/// Write a small synthetic mono `f32` FITS at `path`, for tests in this crate.
///
/// Deliberately not a flat field: the auto-stretch derives its coefficients from
/// the data's own distribution, so a constant image would exercise a degenerate
/// path that no real frame takes. This is a gradient with a few bright points.
#[cfg(test)]
pub(crate) fn write_test_fits(path: &Path, width: usize, height: usize) {
    let mut data = vec![0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let v = (x as f32 / width as f32) * 900.0 + (y as f32 / height as f32) * 100.0;
            data[y * width + x] = v;
        }
    }
    // A handful of "stars" so the stretch has real highlights to pull.
    for i in 0..8 {
        let x = (i * 7 + 3) % width;
        let y = (i * 11 + 5) % height;
        data[y * width + x] = 30000.0;
    }
    athenaeum_core::fits_writer::write_fits_f32(path, width, height, 1, &data, &[])
        .expect("write test fits");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a baseline JPEG's true dimensions out of its SOF0 marker, so the
    /// downscale assertions test the actual pixels rather than our own maths.
    fn jpeg_dimensions(bytes: &[u8]) -> (usize, usize) {
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "SOI");
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            assert_eq!(bytes[i], 0xFF, "marker at {i}");
            let marker = bytes[i + 1];
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            // SOF0/SOF1/SOF2: height then width, both big-endian u16.
            if matches!(marker, 0xC0..=0xC2) {
                let h = usize::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
                let w = usize::from(u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]));
                return (w, h);
            }
            i += 2 + len;
        }
        panic!("no SOF marker in {} bytes", bytes.len());
    }

    fn fixture(dir: &Path, name: &str, w: usize, h: usize) -> PathBuf {
        let p = dir.join(name);
        write_test_fits(&p, w, h);
        std::fs::canonicalize(&p).unwrap()
    }

    /// The core contract: a real FITS renders to real JPEG bytes at the requested
    /// width, and asking again does NOT re-render.
    #[tokio::test]
    async fn renders_jpeg_and_serves_the_second_request_from_cache() {
        let tmp = tempfile::tempdir().unwrap();
        // 256x192 mono → rustafits preview binning halves it to 128x96.
        let f = fixture(tmp.path(), "light.fits", 256, 192);
        let cache = PreviewCache::new();

        let key = PreviewKey::stat(&f, 64).unwrap();
        let bytes = render_jpeg(&cache, &key, &f).await.unwrap();
        assert_eq!(&bytes[0..3], &[0xFF, 0xD8, 0xFF], "JPEG magic bytes");
        assert_eq!(
            &bytes[bytes.len() - 2..],
            &[0xFF, 0xD9],
            "complete JPEG (EOI)"
        );
        assert_eq!(cache.render_count(), 1);
        // 128x96 binned, box-downscaled to width 64 → 64x48.
        assert_eq!(jpeg_dimensions(&bytes), (64, 48));

        let again = render_jpeg(&cache, &key, &f).await.unwrap();
        assert_eq!(
            cache.render_count(),
            1,
            "second request must be a cache hit"
        );
        assert!(
            Arc::ptr_eq(&bytes, &again),
            "the very same buffer is served"
        );
    }

    /// A width above [`MAX_WIDTH`] is clamped ONCE, in the key — so an absurd
    /// `?w=` and the cap are literally the same request: same ETag, one render.
    #[tokio::test]
    async fn width_is_clamped_in_the_key_not_just_at_render_time() {
        assert_eq!(clamp_width(9999), MAX_WIDTH);
        assert_eq!(clamp_width(0), MIN_WIDTH);
        assert_eq!(clamp_width(3), MIN_WIDTH);
        assert_eq!(clamp_width(640), 640);

        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "light.fits", 256, 192);
        let huge = PreviewKey::stat(&f, 9999).unwrap();
        let capped = PreviewKey::stat(&f, MAX_WIDTH).unwrap();
        assert_eq!(huge.width(), MAX_WIDTH);
        assert_eq!(huge, capped);
        assert_eq!(huge.etag(), capped.etag(), "one ETag, not two");

        let cache = PreviewCache::new();
        render_jpeg(&cache, &huge, &f).await.unwrap();
        render_jpeg(&cache, &capped, &f).await.unwrap();
        assert_eq!(cache.render_count(), 1, "clamped to the same cache entry");
    }

    /// The source is smaller than the request: never upscaled, and the JPEG
    /// carries rustafits's own (binned) geometry.
    #[tokio::test]
    async fn never_upscales_a_small_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "small.fits", 256, 192);
        let cache = PreviewCache::new();
        let key = PreviewKey::stat(&f, MAX_WIDTH).unwrap();
        let bytes = render_jpeg(&cache, &key, &f).await.unwrap();
        assert_eq!(jpeg_dimensions(&bytes), (128, 96), "binned, not blown up");
    }

    /// Rewriting the file under the same name must invalidate: the key carries
    /// size + mtime precisely so a re-saved sub is not served from the old bytes.
    #[tokio::test]
    async fn a_rewritten_file_gets_a_new_key_and_a_new_render() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "light.fits", 256, 192);
        let cache = PreviewCache::new();
        let before = PreviewKey::stat(&f, 128).unwrap();
        render_jpeg(&cache, &before, &f).await.unwrap();

        write_test_fits(&f, 320, 240); // different geometry → different size
        let after = PreviewKey::stat(&f, 128).unwrap();
        assert_ne!(before, after);
        assert_ne!(before.etag(), after.etag());
        render_jpeg(&cache, &after, &f).await.unwrap();
        assert_eq!(cache.render_count(), 2);
    }

    /// The LRU is bounded, and it evicts the LEAST-recently-used entry — not
    /// the oldest-inserted one.
    #[tokio::test]
    async fn lru_is_bounded_and_evicts_least_recently_used() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "light.fits", 128, 96);
        let cache = PreviewCache::new();

        let keys: Vec<_> = (0..CACHE_CAP)
            .map(|i| PreviewKey::stat(&f, MIN_WIDTH + i as u32).unwrap())
            .collect();
        for k in &keys {
            render_jpeg(&cache, k, &f).await.unwrap();
        }
        assert_eq!(cache.len(), CACHE_CAP);
        assert_eq!(cache.render_count(), CACHE_CAP);

        // Touch the oldest so it is no longer the eviction victim, then overflow.
        render_jpeg(&cache, &keys[0], &f).await.unwrap();
        assert_eq!(cache.render_count(), CACHE_CAP, "that was a hit");
        let extra = PreviewKey::stat(&f, MIN_WIDTH + CACHE_CAP as u32).unwrap();
        render_jpeg(&cache, &extra, &f).await.unwrap();

        assert_eq!(cache.len(), CACHE_CAP, "still bounded");
        assert!(
            cache.entries.get(&keys[0]).is_some(),
            "recently used one survived"
        );
        assert!(
            cache.entries.get(&keys[1]).is_none(),
            "the LRU one was evicted"
        );
    }

    /// Two viewers open the same frame at once. The second must ride the first's
    /// result rather than queue a duplicate stretch — the post-gate cache
    /// re-check earning its keep.
    #[tokio::test]
    async fn concurrent_requests_for_one_frame_render_once() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "light.fits", 256, 192);
        let cache = PreviewCache::new();
        let key = PreviewKey::stat(&f, 128).unwrap();

        let (a, b) = tokio::join!(render_jpeg(&cache, &key, &f), render_jpeg(&cache, &key, &f));
        assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()), "one shared buffer");
        assert_eq!(cache.render_count(), 1, "one render, not two");
    }

    /// An abandoned request (the browser swapped the `<img>` src mid-render) must
    /// not leave the gate held forever. The permit lives on the blocking task, so
    /// the next render waits for that work to finish — and then proceeds.
    #[tokio::test]
    async fn an_abandoned_render_does_not_wedge_the_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fixture(tmp.path(), "light.fits", 256, 192);
        let cache = Arc::new(PreviewCache::new());
        let key = PreviewKey::stat(&f, 128).unwrap();

        let task = {
            let (c, k, p) = (Arc::clone(&cache), key.clone(), f.clone());
            tokio::spawn(async move { render_jpeg(&c, &k, &p).await.map(|_| ()) })
        };
        tokio::task::yield_now().await; // let it reach the gate
        task.abort();
        let _ = task.await;

        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            render_jpeg(&cache, &key, &f),
        )
        .await
        .expect("the gate must be released, not wedged")
        .unwrap();
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8]);
    }

    /// A file that is not an image at all fails as an error, not a panic — the
    /// route turns this into a `422`.
    #[tokio::test]
    async fn a_corrupt_file_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("junk.fits");
        std::fs::write(&p, b"this is not a FITS file at all").unwrap();
        let p = std::fs::canonicalize(&p).unwrap();
        let cache = PreviewCache::new();
        let key = PreviewKey::stat(&p, 200).unwrap();
        assert!(render_jpeg(&cache, &key, &p).await.is_err());
    }

    /// A vanished file reports the library contract's `"not found"` prefix, which
    /// is what maps it onto a `404` in the route.
    #[test]
    fn stat_of_a_missing_file_uses_the_not_found_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let err = PreviewKey::stat(&tmp.path().join("gone.fits"), 200)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not found"), "got {err:?}");
    }

    /// Box-downscale maths, independent of any decoder: a 4×4 two-channel ramp
    /// halved is the mean of each 2×2 block, per channel.
    #[test]
    fn box_downscale_averages_each_block_per_channel() {
        let ch = 2;
        let (sw, sh) = (4usize, 4usize);
        let mut src = vec![0u8; sw * sh * ch];
        for y in 0..sh {
            for x in 0..sw {
                src[(y * sw + x) * ch] = (y * sw + x) as u8; // 0..15
                src[(y * sw + x) * ch + 1] = 100;
            }
        }
        let out = box_downscale(&src, sw, sh, ch, 2, 2);
        // Top-left block = mean(0,1,4,5) = 2; top-right = mean(2,3,6,7) = 4.
        assert_eq!(out[0], 2);
        assert_eq!(out[2], 4);
        // Bottom-left = mean(8,9,12,13) = 10; bottom-right = mean(10,11,14,15) = 12.
        assert_eq!(out[4], 10);
        assert_eq!(out[6], 12);
        // The constant channel survives untouched.
        assert!(out.iter().skip(1).step_by(2).all(|&v| v == 100));
    }

    /// A source narrower than the target is passed through borrowed — no copy,
    /// no upscale.
    #[test]
    fn fit_to_width_borrows_when_no_downscale_is_needed() {
        let src = vec![7u8; 10 * 5 * 3];
        let (cow, w, h) = fit_to_width(&src, 10, 5, 3, 400);
        assert!(matches!(cow, Cow::Borrowed(_)));
        assert_eq!((w, h), (10, 5));
    }

    /// ETags must differ across every field of the key, and the variable-length
    /// path must not be able to bleed into the fixed tail.
    #[test]
    fn etag_discriminates_every_key_field() {
        let mk = |id: &str, size: u64, mtime: i64, width: u32| PreviewKey {
            id: id.to_string(),
            size,
            mtime_ms: mtime,
            width,
        };
        let base = mk("/cap/a.fits", 10, 20, 640);
        for other in [
            mk("/cap/b.fits", 10, 20, 640),
            mk("/cap/a.fits", 11, 20, 640),
            mk("/cap/a.fits", 10, 21, 640),
            mk("/cap/a.fits", 10, 20, 641),
            mk("/cap/a.fit", 10, 20, 640),
        ] {
            assert_ne!(base.etag(), other.etag(), "{other:?}");
        }
        assert_eq!(base.etag(), mk("/cap/a.fits", 10, 20, 640).etag());
        assert!(
            base.etag().starts_with('"') && base.etag().ends_with('"'),
            "strong ETags are quoted: {}",
            base.etag()
        );
    }
}
