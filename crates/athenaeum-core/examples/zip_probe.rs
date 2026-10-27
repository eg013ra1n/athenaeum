//! Phase-0 probe for the ZIP-archives design (spec 2026-10-27-zip-archives-in-catalog-design.md §12):
//! what reading a zip's metadata and extracting one entry costs on a real share.
//!
//! For each zip: central-directory read, then the header of every FITS/XISF entry (decompress from the
//! entry start until the FITS END card or the XISF header length, then stop), then a full extraction of
//! one entry (CRC checked by the zip reader, xxh3 streamed alongside). Every byte pulled from the file is
//! counted below the buffer, so "bytes read" is what crossed the network.
//!
//! cargo run --release -p athenaeum-core --example zip_probe -- [--buf KIB] [--extract-to DIR] ZIP...

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Instant;
use xxhash_rust::xxh3::Xxh3;

const EXTRACT_BUF_KIB: usize = 1024;

/// Bytes, read calls and seeks that reach the underlying file.
#[derive(Default)]
struct Stats {
    bytes: AtomicU64,
    reads: AtomicU64,
    seeks: AtomicU64,
}

impl Stats {
    fn get(&self) -> (u64, u64, u64) {
        (self.bytes.load(Relaxed), self.reads.load(Relaxed), self.seeks.load(Relaxed))
    }
}

struct Counting {
    inner: File,
    stats: Arc<Stats>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.stats.bytes.fetch_add(n as u64, Relaxed);
        self.stats.reads.fetch_add(1, Relaxed);
        Ok(n)
    }
}

impl Seek for Counting {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.stats.seeks.fetch_add(1, Relaxed);
        self.inner.seek(pos)
    }
}

fn open(path: &Path, buf_kib: usize, stats: &Arc<Stats>) -> Result<BufReader<Counting>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    // Bypass the local page cache so a re-run still measures reads from the share, not from RAM.
    #[cfg(target_os = "macos")]
    unsafe {
        use std::os::fd::AsRawFd;
        libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1);
    }
    Ok(BufReader::with_capacity(buf_kib * 1024, Counting { inner: f, stats: stats.clone() }))
}

fn is_frame(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["fits", "fit", "fts", "xisf"].iter().any(|e| lower.ends_with(&format!(".{e}")))
}

/// Read decompressed bytes until the header is complete; returns the decompressed header length.
fn read_header(entry: &mut impl Read, name: &str) -> Result<usize> {
    let mut buf = Vec::with_capacity(64 * 1024);
    if name.to_ascii_lowercase().ends_with(".xisf") {
        let mut sig = [0u8; 16];
        entry.read_exact(&mut sig)?;
        if &sig[..8] != b"XISF0100" {
            bail!("{name}: not an XISF signature");
        }
        let len = u32::from_le_bytes([sig[8], sig[9], sig[10], sig[11]]) as usize;
        let mut hdr = vec![0u8; len];
        entry.read_exact(&mut hdr)?;
        return Ok(16 + len);
    }
    let mut block = [0u8; 2880];
    loop {
        entry.read_exact(&mut block).with_context(|| format!("{name}: header ended without END"))?;
        buf.extend_from_slice(&block);
        if block.chunks(80).any(|card| card.starts_with(b"END") && card[3..].iter().all(|&b| b == b' ')) {
            return Ok(buf.len());
        }
        if buf.len() > 2880 * 200 {
            bail!("{name}: no END card in 200 blocks");
        }
    }
}

fn mib(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn probe(zip_path: &Path, buf_kib: usize, extract_to: Option<&Path>) -> Result<()> {
    let zip_size = std::fs::metadata(zip_path)?.len();
    println!("== {} ({:.1} MiB), header buffer {} KiB, extract buffer {} KiB", zip_path.display(), mib(zip_size), buf_kib, EXTRACT_BUF_KIB);

    // 1. Central directory.
    let t = Instant::now();
    let stats = Arc::new(Stats::default());
    let mut zr = zip::ZipArchive::new(open(zip_path, buf_kib, &stats)?).context("read central directory")?;
    let cd_ms = t.elapsed().as_secs_f64() * 1e3;
    let (b, r, s) = stats.get();
    let names: Vec<String> = (0..zr.len()).filter_map(|i| zr.name_for_index(i).map(str::to_string)).collect();
    let frames: Vec<usize> = (0..names.len()).filter(|&i| is_frame(&names[i])).collect();
    println!("central directory: {cd_ms:.0} ms, {b} bytes in {r} reads / {s} seeks; {} entries, {} frames", names.len(), frames.len());

    // 2. Header of every frame entry.
    let t = Instant::now();
    let (b0, r0, s0) = stats.get();
    let mut methods = std::collections::BTreeMap::<String, usize>::new();
    let mut hdr_total = 0usize;
    let mut largest: Option<(usize, u64)> = None;
    for &i in &frames {
        let mut entry = zr.by_index(i)?;
        *methods.entry(format!("{:?}", entry.compression())).or_default() += 1;
        if largest.is_none_or(|(_, sz)| entry.size() > sz) {
            largest = Some((i, entry.size()));
        }
        let name = entry.name().to_string();
        hdr_total += read_header(&mut entry, &name)?;
    }
    let hdr_ms = t.elapsed().as_secs_f64() * 1e3;
    let (b1, r1, s1) = stats.get();
    let hb = b1 - b0;
    println!(
        "headers: {hdr_ms:.0} ms ({:.1} ms/frame), {:.2} MiB read in {} reads / {} seeks = {:.3}% of zip; \
         {:.0} KiB read per frame for {:.1} KiB of header; methods {methods:?}",
        hdr_ms / frames.len().max(1) as f64,
        mib(hb),
        r1 - r0,
        s1 - s0,
        100.0 * hb as f64 / zip_size as f64,
        hb as f64 / 1024.0 / frames.len().max(1) as f64,
        hdr_total as f64 / 1024.0 / frames.len().max(1) as f64,
    );

    // 3. Full extraction of the largest frame entry.
    // Extraction reads through its own archive handle with a large buffer: small reads suit the header
    // pass, but stream a whole entry at a few MiB/s over a share.
    let Some((idx, size)) = largest else { return Ok(()) };
    let stats = Arc::new(Stats::default());
    let mut zr = zip::ZipArchive::new(open(zip_path, EXTRACT_BUF_KIB, &stats)?)?;
    let t = Instant::now();
    let (b0, _, _) = stats.get();
    let mut entry = zr.by_index(idx)?;
    let name = entry.name().to_string();
    let compressed = entry.compressed_size();
    let mut out: Option<File> = match extract_to {
        Some(dir) => Some(File::create(dir.join(Path::new(&name).file_name().unwrap_or_default()))?),
        None => None,
    };
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        // The zip reader checks the CRC32 at the end of the entry and errors on a mismatch.
        let n = entry.read(&mut buf).with_context(|| format!("extract {name}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if let Some(f) = out.as_mut() {
            std::io::Write::write_all(f, &buf[..n])?;
        }
        total += n as u64;
    }
    drop(entry);
    let secs = t.elapsed().as_secs_f64();
    let (b1, _, _) = stats.get();
    println!(
        "extract {name}: {:.1} MiB ({:.1} MiB compressed, ratio {:.2}) in {secs:.2} s = {:.1} MiB/s of entry, \
         {:.1} MiB/s off the share; CRC ok, xxh3 {:016x}",
        mib(total),
        mib(compressed),
        compressed as f64 / size.max(1) as f64,
        mib(total) / secs,
        mib(b1 - b0) / secs,
        hasher.digest(),
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut buf_kib = 64usize;
    let mut extract_to: Option<PathBuf> = None;
    let mut zips = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--buf" => buf_kib = args.next().context("--buf KIB")?.parse()?,
            "--extract-to" => extract_to = Some(PathBuf::from(args.next().context("--extract-to DIR")?)),
            _ => zips.push(PathBuf::from(a)),
        }
    }
    if zips.is_empty() {
        bail!("usage: zip_probe [--buf KIB] [--extract-to DIR] ZIP...");
    }
    for z in &zips {
        if let Err(e) = probe(z, buf_kib, extract_to.as_deref()) {
            eprintln!("{}: {e:#}", z.display());
        }
    }
    Ok(())
}
