//! HiPS (IVOA Hierarchical Progressive Survey) image tiles for the Sky
//! Chart background (issue #18).
//!
//! A survey is a fixed preset: no arbitrary URL reaches this module. Tiles
//! are fetched from the survey's master on demand and kept in a private
//! per-install store under `<app_data>/hips/<survey-id>/`, in the standard
//! HiPS layout. Tiles are never redistributed (docs/skychart/README.md,
//! "HiPS"). A tile is addressed only by validated numbers (survey id,
//! order, npix), so no frontend path ever reaches the filesystem.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Tile image format of a survey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFormat {
    Jpeg,
    Png,
}

impl TileFormat {
    pub fn ext(self) -> &'static str {
        match self {
            TileFormat::Jpeg => "jpg",
            TileFormat::Png => "png",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            TileFormat::Jpeg => "image/jpeg",
            TileFormat::Png => "image/png",
        }
    }

    /// The `hips_tile_format` word for this format.
    pub fn properties_word(self) -> &'static str {
        match self {
            TileFormat::Jpeg => "jpeg",
            TileFormat::Png => "png",
        }
    }

    /// True when `bytes` start with this format's signature.
    pub fn matches(self, bytes: &[u8]) -> bool {
        match self {
            TileFormat::Jpeg => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
            TileFormat::Png => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        }
    }
}

/// One survey preset.
#[derive(Debug)]
pub struct HipsSurvey {
    /// Stable id; also the store directory name.
    pub id: &'static str,
    pub label: &'static str,
    /// The survey's master URL (never a mirror), without a trailing slash.
    pub base_url: &'static str,
    pub format: TileFormat,
    /// The survey's deepest order (`hips_order`).
    pub max_order: u8,
    /// Typical size of one tile in bytes; used for estimates when the
    /// survey's own `properties` have not been stored yet.
    pub approx_tile_bytes: u64,
    /// Shown until the survey's own `properties` have been read.
    pub fallback_attribution: &'static str,
    pub fallback_license: &'static str,
}

/// DSS colored, CDS. Live `properties` checked 2026-10-26: equatorial,
/// 512 px JPEG, order 9, `hips_status = public master clonableOnce`.
pub const DSS_COLOR: HipsSurvey = HipsSurvey {
    id: "dss-color",
    label: "DSS colored",
    base_url: "https://alasky.cds.unistra.fr/DSS/DSSColor",
    format: TileFormat::Jpeg,
    max_order: 9,
    approx_tile_bytes: 67_000,
    fallback_attribution: "Digitized Sky Survey - STScI/NASA, Colored & Healpixed by CDS",
    fallback_license: "ODbL-1.0",
};

pub const SURVEYS: &[HipsSurvey] = &[DSS_COLOR];

/// Every survey is 512 px tiles; a survey whose `properties` say otherwise is refused.
pub const TILE_WIDTH: u32 = 512;
/// The chart never asks for a deeper order (≈ 6.4″ per tile pixel; see docs/skychart).
pub const DISPLAY_MAX_ORDER: u8 = 6;
/// A tile bigger than this is not a tile.
pub const MAX_TILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROPERTIES_BYTES: u64 = 256 * 1024;

pub fn survey(id: &str) -> Option<&'static HipsSurvey> {
    SURVEYS.iter().find(|s| s.id == id)
}

/// The highest order the chart may request from `survey`.
pub fn request_max_order(survey: &HipsSurvey) -> u8 {
    survey.max_order.min(DISPLAY_MAX_ORDER)
}

/// Checks a tile address against the survey and the HEALPix pixel count.
pub fn validate_address(survey: &HipsSurvey, order: u8, npix: u64) -> Result<()> {
    let max = request_max_order(survey);
    if order > max {
        bail!("HiPS order {order} is above {max} for {}", survey.id);
    }
    let n = 12u64 << (2 * u32::from(order));
    if npix >= n {
        bail!("HiPS pixel {npix} is out of range for order {order} (< {n})");
    }
    Ok(())
}

/// `Norder{k}/Dir{(npix / 10000) * 10000}/Npix{npix}.{ext}` (HiPS 1.0 §4.1).
pub fn tile_rel_path(order: u8, npix: u64, format: TileFormat) -> PathBuf {
    PathBuf::from(format!("Norder{order}"))
        .join(format!("Dir{}", (npix / 10_000) * 10_000))
        .join(format!("Npix{npix}.{}", format.ext()))
}

/// A parsed HiPS `properties` file (`key = value` lines, `#` comments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Properties(pub BTreeMap<String, String>);

impl Properties {
    pub fn parse(text: &str) -> Self {
        let mut map = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Properties(map)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Refuses a survey the renderer cannot draw correctly: another sky
    /// frame, tile size or format.
    pub fn validate(&self, survey: &HipsSurvey) -> Result<()> {
        let frame = self.get("hips_frame").unwrap_or("equatorial");
        if frame != "equatorial" {
            bail!("{}: hips_frame is {frame}, only equatorial is supported", survey.id);
        }
        let width = self.get("hips_tile_width").unwrap_or("512");
        if width.parse::<u32>().ok() != Some(TILE_WIDTH) {
            bail!("{}: hips_tile_width is {width}, expected {TILE_WIDTH}", survey.id);
        }
        let formats = self.get("hips_tile_format").unwrap_or("");
        if !formats.split_whitespace().any(|f| f == survey.format.properties_word()) {
            bail!("{}: hips_tile_format \"{formats}\" has no {}", survey.id, survey.format.properties_word());
        }
        if let Some(order) = self.get("hips_order") {
            match order.parse::<u8>() {
                Ok(o) if o >= request_max_order(survey) => {}
                _ => bail!("{}: hips_order is {order}, the chart needs {}", survey.id, request_max_order(survey)),
            }
        }
        Ok(())
    }
}

/// What the store holds for a tile.
#[derive(Debug, PartialEq, Eq)]
pub enum CachedTile {
    Image(Vec<u8>),
    /// The master answered 404: no coverage there; never asked again.
    Absent,
    Missing,
}

/// The private tile store of one survey.
#[derive(Debug, Clone)]
pub struct HipsStore {
    root: PathBuf,
    format: TileFormat,
}

static PART_COUNTER: AtomicU64 = AtomicU64::new(0);

impl HipsStore {
    pub fn new(app_data: &Path, survey: &HipsSurvey) -> Self {
        HipsStore { root: app_data.join("hips").join(survey.id), format: survey.format }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn tile_path(&self, order: u8, npix: u64) -> PathBuf {
        self.root.join(tile_rel_path(order, npix, self.format))
    }

    fn absent_path(&self, order: u8, npix: u64) -> PathBuf {
        self.tile_path(order, npix).with_extension("absent")
    }

    /// Reads a tile. A cached file that is not a valid image is deleted and
    /// reported as missing, so the next request fetches it again.
    pub fn read(&self, order: u8, npix: u64) -> Result<CachedTile> {
        let path = self.tile_path(order, npix);
        match fs::read(&path) {
            Ok(bytes) if self.format.matches(&bytes) && bytes.len() as u64 <= MAX_TILE_BYTES => {
                return Ok(CachedTile::Image(bytes));
            }
            Ok(_) => {
                tracing::warn!(path = %path.display(), "cached hips tile is not a valid image; removing it");
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
                return Ok(CachedTile::Missing);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        }
        if self.absent_path(order, npix).exists() {
            return Ok(CachedTile::Absent);
        }
        Ok(CachedTile::Missing)
    }

    /// Writes a tile atomically (`.part` + rename), so a reader never sees half a file.
    pub fn write(&self, order: u8, npix: u64, bytes: &[u8]) -> Result<()> {
        write_atomic(&self.tile_path(order, npix), bytes)
    }

    pub fn mark_absent(&self, order: u8, npix: u64) -> Result<()> {
        write_atomic(&self.absent_path(order, npix), b"")
    }

    pub fn read_properties(&self) -> Result<Option<Properties>> {
        let path = self.root.join("properties");
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Properties::parse(&text))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Keeps the survey's original `properties` text next to its tiles.
    pub fn write_properties(&self, text: &str) -> Result<()> {
        write_atomic(&self.root.join("properties"), text.as_bytes())
    }

    /// Tile count and bytes on disk (absent markers and `properties` excluded).
    pub fn usage(&self) -> Result<(u64, u64)> {
        let mut tiles = 0;
        let mut bytes = 0;
        let ext = self.format.ext();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("list {}", dir.display())),
            };
            for entry in entries {
                let entry = entry?;
                let ty = entry.file_type()?;
                if ty.is_dir() {
                    stack.push(entry.path());
                } else if ty.is_file() && entry.path().extension().is_some_and(|e| e == ext) {
                    tiles += 1;
                    bytes += entry.metadata()?.len();
                }
            }
        }
        Ok((tiles, bytes))
    }

    /// Deletes the whole store; returns the tile bytes it held.
    pub fn clear(&self) -> Result<u64> {
        let (_, bytes) = self.usage()?;
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).with_context(|| format!("remove {}", self.root.display())),
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("tile path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let n = PART_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().context("tile path has no file name")?.to_string_lossy();
    let part = dir.join(format!(".{file_name}.{}.{n}.part", std::process::id()));
    fs::write(&part, bytes).with_context(|| format!("write {}", part.display()))?;
    if let Err(e) = fs::rename(&part, path) {
        let _ = fs::remove_file(&part);
        return Err(e).with_context(|| format!("rename into {}", path.display()));
    }
    Ok(())
}

/// What the survey's master answered for a tile.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchedTile {
    Image(Vec<u8>),
    Absent,
}

fn client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("Athenaeum/", env!("CARGO_PKG_VERSION"), " (+https://artfrom.space)"))
        .build()
        .context("build HiPS HTTP client")?;
    Ok(CLIENT.get_or_init(|| c))
}

/// The survey's master answered 429 or 503: it is busy, not broken.
/// Callers detect it with `err.downcast_ref::<ServerBusy>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerBusy {
    /// The `Retry-After` delta-seconds, when the server sent one. An
    /// HTTP-date form is not honoured and reads as `None`.
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for ServerBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.retry_after {
            Some(d) => write!(f, "survey server busy, retry after {} s", d.as_secs()),
            None => write!(f, "survey server busy"),
        }
    }
}

impl std::error::Error for ServerBusy {}

fn get_bounded(url: &str, cap: u64) -> Result<Option<Vec<u8>>> {
    let resp = client()?.get(url).send().with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        return Err(anyhow::Error::new(ServerBusy { retry_after }).context(format!("GET {url}: HTTP {status}")));
    }
    if !status.is_success() {
        bail!("GET {url}: HTTP {status}");
    }
    let mut body = Vec::new();
    resp.take(cap + 1).read_to_end(&mut body).with_context(|| format!("read {url}"))?;
    if body.len() as u64 > cap {
        bail!("GET {url}: response larger than {cap} bytes");
    }
    Ok(Some(body))
}

/// Fetches the survey's `properties` text from its master.
pub fn fetch_properties(survey: &HipsSurvey) -> Result<String> {
    fetch_properties_from(survey.base_url, survey.id)
}

fn fetch_properties_from(base_url: &str, id: &str) -> Result<String> {
    let url = format!("{base_url}/properties");
    let body = get_bounded(&url, MAX_PROPERTIES_BYTES)?
        .with_context(|| format!("{id}: no properties file at {url}"))?;
    String::from_utf8(body).with_context(|| format!("{id}: properties is not UTF-8"))
}

/// Fetches one tile from the survey's master.
pub fn fetch_tile(survey: &HipsSurvey, order: u8, npix: u64) -> Result<FetchedTile> {
    fetch_tile_from(survey.base_url, survey.format, order, npix)
}

pub(crate) fn fetch_tile_from(base_url: &str, format: TileFormat, order: u8, npix: u64) -> Result<FetchedTile> {
    let rel = tile_rel_path(order, npix, format);
    let url = format!("{base_url}/{}", rel.to_string_lossy().replace('\\', "/"));
    match get_bounded(&url, MAX_TILE_BYTES)? {
        None => Ok(FetchedTile::Absent),
        Some(body) if format.matches(&body) => Ok(FetchedTile::Image(body)),
        Some(_) => bail!("GET {url}: the response is not a {} image", format.properties_word()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

    #[test]
    fn tile_paths_follow_the_hips_layout() {
        assert_eq!(tile_rel_path(3, 7, TileFormat::Jpeg), PathBuf::from("Norder3/Dir0/Npix7.jpg"));
        assert_eq!(tile_rel_path(6, 33_000, TileFormat::Png), PathBuf::from("Norder6/Dir30000/Npix33000.png"));
        assert_eq!(tile_rel_path(5, 9_999, TileFormat::Jpeg), PathBuf::from("Norder5/Dir0/Npix9999.jpg"));
        assert_eq!(tile_rel_path(5, 10_000, TileFormat::Jpeg), PathBuf::from("Norder5/Dir10000/Npix10000.jpg"));
    }

    #[test]
    fn addresses_are_bounded() {
        assert!(validate_address(&DSS_COLOR, 0, 11).is_ok());
        assert!(validate_address(&DSS_COLOR, 0, 12).is_err());
        assert!(validate_address(&DSS_COLOR, 6, 12 * 4096 - 1).is_ok());
        assert!(validate_address(&DSS_COLOR, 6, 12 * 4096).is_err());
        assert!(validate_address(&DSS_COLOR, 7, 0).is_err(), "deeper than the chart ever asks");
        assert!(survey("dss-color").is_some());
        assert!(survey("../etc").is_none());
    }

    #[test]
    fn properties_parse_and_validate() {
        let live = "hips_order           = 9\n# comment\nhips_frame = equatorial\nhips_tile_width = 512\nhips_tile_format     = jpeg\nobs_copyright = A = B\n";
        let p = Properties::parse(live);
        assert_eq!(p.get("obs_copyright"), Some("A = B"));
        assert!(p.validate(&DSS_COLOR).is_ok());

        for bad in [
            live.replace("equatorial", "galactic"),
            live.replace("= 512", "= 256"),
            live.replace("jpeg", "png fits"),
            live.replace("= 9", "= 4"),
        ] {
            assert!(Properties::parse(&bad).validate(&DSS_COLOR).is_err(), "{bad}");
        }
        assert!(Properties::parse(&live.replace("jpeg", "png jpeg")).validate(&DSS_COLOR).is_ok());
    }

    #[test]
    fn formats_are_recognised_by_signature() {
        assert!(TileFormat::Jpeg.matches(JPEG));
        assert!(!TileFormat::Jpeg.matches(b"<html>"));
        assert!(TileFormat::Png.matches(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]));
    }

    #[test]
    fn store_round_trip_absent_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = HipsStore::new(dir.path(), &DSS_COLOR);
        assert_eq!(store.read(3, 7).unwrap(), CachedTile::Missing);
        store.write(3, 7, JPEG).unwrap();
        assert_eq!(store.read(3, 7).unwrap(), CachedTile::Image(JPEG.to_vec()));
        assert!(dir.path().join("hips/dss-color/Norder3/Dir0/Npix7.jpg").is_file());

        store.mark_absent(3, 8).unwrap();
        assert_eq!(store.read(3, 8).unwrap(), CachedTile::Absent);

        fs::write(dir.path().join("hips/dss-color/Norder3/Dir0/Npix9.jpg"), b"<html>error</html>").unwrap();
        assert_eq!(store.read(3, 9).unwrap(), CachedTile::Missing);
        assert!(!dir.path().join("hips/dss-color/Norder3/Dir0/Npix9.jpg").exists(), "a corrupt tile is removed");

        assert_eq!(store.usage().unwrap(), (1, JPEG.len() as u64));
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("hips/dss-color/Norder3/Dir0"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());

        store.write_properties("hips_frame = equatorial\n").unwrap();
        assert_eq!(store.read_properties().unwrap().unwrap().get("hips_frame"), Some("equatorial"));
        assert_eq!(store.clear().unwrap(), JPEG.len() as u64);
        assert!(!store.root().exists());
        assert_eq!(store.clear().unwrap(), 0);
    }

    /// A one-shot local HTTP server answering each request from `routes`.
    fn serve(routes: Vec<(&'static str, u16, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(routes.len()) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, Vec::new()));
                let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        format!("http://{addr}")
    }

    /// A one-shot local HTTP server answering the first request with a
    /// status line and extra header lines.
    fn serve_status(status: &str, extra_headers: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let head = format!("HTTP/1.1 {status}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        std::thread::spawn(move || {
            if let Some(stream) = listener.incoming().next() {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf).unwrap();
                stream.write_all(head.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}")
    }

    fn busy_of(err: &anyhow::Error) -> Option<&ServerBusy> {
        err.downcast_ref::<ServerBusy>()
    }

    #[test]
    fn busy_429_carries_retry_after() {
        let base = serve_status("429 Too Many Requests", "Retry-After: 7\r\n");
        let err = fetch_tile_from(&base, TileFormat::Jpeg, 3, 7).unwrap_err();
        assert_eq!(busy_of(&err).unwrap().retry_after, Some(Duration::from_secs(7)));
    }

    #[test]
    fn busy_503_without_header() {
        let base = serve_status("503 Service Unavailable", "");
        let err = fetch_tile_from(&base, TileFormat::Jpeg, 3, 7).unwrap_err();
        assert_eq!(busy_of(&err).unwrap().retry_after, None);
    }

    #[test]
    fn retry_after_http_date_is_ignored() {
        let base = serve_status("503 Service Unavailable", "Retry-After: Wed, 21 Oct 2026 07:28:00 GMT\r\n");
        let err = fetch_tile_from(&base, TileFormat::Jpeg, 3, 7).unwrap_err();
        assert_eq!(busy_of(&err).unwrap().retry_after, None);
    }

    #[test]
    fn other_5xx_is_not_busy() {
        let base = serve_status("500 Internal Server Error", "");
        let err = fetch_tile_from(&base, TileFormat::Jpeg, 3, 7).unwrap_err();
        assert!(busy_of(&err).is_none());
        assert!(format!("{err:#}").contains("HTTP 500"), "{err:#}");
    }

    #[test]
    fn fetch_maps_status_and_body() {
        let base = serve(vec![
            ("/Norder3/Dir0/Npix7.jpg", 200, JPEG.to_vec()),
            ("/Norder3/Dir0/Npix8.jpg", 404, Vec::new()),
            ("/Norder3/Dir0/Npix9.jpg", 200, b"<html>".to_vec()),
            ("/Norder3/Dir0/Npix10.jpg", 503, Vec::new()),
            ("/properties", 200, b"hips_frame = equatorial\n".to_vec()),
        ]);
        assert_eq!(fetch_tile_from(&base, TileFormat::Jpeg, 3, 7).unwrap(), FetchedTile::Image(JPEG.to_vec()));
        assert_eq!(fetch_tile_from(&base, TileFormat::Jpeg, 3, 8).unwrap(), FetchedTile::Absent);
        assert!(fetch_tile_from(&base, TileFormat::Jpeg, 3, 9).is_err(), "not an image");
        assert!(fetch_tile_from(&base, TileFormat::Jpeg, 3, 10).is_err(), "server error");
        assert_eq!(fetch_properties_from(&base, "t").unwrap(), "hips_frame = equatorial\n");
    }
}

/// Golden vectors pinning the frontend's HEALPix port
/// (`src/components/skychart/healpix.ts`) to cdshealpix. Regenerate with
/// `HIPS_GOLDEN_WRITE=1 cargo test -p athenaeum-core hips::golden`.
#[cfg(test)]
mod golden {
    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../src/components/skychart/healpix.golden.json");

    fn vectors() -> serde_json::Value {
        let mut out = Vec::new();
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for order in 0u8..=6 {
            let n = 12u64 << (2 * u32::from(order));
            let mut cells: Vec<u64> = vec![0, n - 1, n / 2, n / 3];
            cells.extend((0..20).map(|_| next() % n));
            for npix in cells {
                for (dx, dy) in [(0.5, 0.5), (0.0, 0.0), (0.999_999, 0.0), (0.0, 0.999_999), (0.25, 0.8), (0.9, 0.1)] {
                    let (lon, lat) = cdshealpix::nested::sph_coo(order, npix, dx, dy);
                    out.push(serde_json::json!([order, npix, dx, dy, lon, lat]));
                }
            }
        }
        serde_json::json!({
            "source": "cdshealpix::nested::sph_coo",
            "columns": ["order", "npix", "dx", "dy", "lon", "lat"],
            "rows": out,
        })
    }

    #[test]
    fn healpix_golden_vectors() {
        let now = vectors();
        if std::env::var("HIPS_GOLDEN_WRITE").is_ok() {
            std::fs::write(FIXTURE, serde_json::to_string(&now).unwrap() + "\n").unwrap();
            return;
        }
        let stored: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(FIXTURE).expect("golden fixture missing; run with HIPS_GOLDEN_WRITE=1"),
        )
        .unwrap();
        // lon/lat go through the platform libm (macOS arm64 differs from the
        // Linux that wrote the fixture by up to 4 ulp), so they are compared
        // to 1e-12 rad, the tolerance the TS test holds the port to.
        let rows = |v: &serde_json::Value| v["rows"].as_array().cloned().unwrap_or_default();
        let (stored_rows, now_rows) = (rows(&stored), rows(&now));
        assert_eq!(stored_rows.len(), now_rows.len(), "cdshealpix moved; regenerate with HIPS_GOLDEN_WRITE=1");
        for (s, n) in stored_rows.iter().zip(&now_rows) {
            let (s, n) = (s.as_array().unwrap(), n.as_array().unwrap());
            assert_eq!(s[..4], n[..4], "golden inputs moved; regenerate with HIPS_GOLDEN_WRITE=1");
            for i in 4..6 {
                let (a, b) = (s[i].as_f64().unwrap(), n[i].as_f64().unwrap());
                assert!(
                    (a - b).abs() <= 1e-12,
                    "cdshealpix moved at {s:?} vs {n:?}; regenerate with HIPS_GOLDEN_WRITE=1 and re-run the TS test"
                );
            }
        }
    }
}

