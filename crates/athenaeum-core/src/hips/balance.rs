//! Spreads tile requests over a survey's master and its verified mirrors
//! (issue #18, phase 3; the author asked that every known mirror be used).
//!
//! - **Choice.** Among the endpoints that are up, the one with the fewest
//!   requests in flight, ties broken by the lower latency (EWMA). An endpoint
//!   never measured counts as the fastest, so a new mirror gets tried.
//! - **Errors.** A request error marks the endpoint down for 30 s, doubling
//!   up to 10 min; the tile is tried on the next endpoint.
//! - **Busy.** A 429 / 503 marks the endpoint down for its `Retry-After`
//!   ([5 s, 300 s], else 30 s). Only when every endpoint is busy does the
//!   caller see [`ServerBusy`], so the pack's busy wait runs then and only then.
//! - **404.** A mirror may be partial: its 404 sends the tile to the master.
//!   Only the master's 404, or every mirror's when the master is down, is an
//!   absent tile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};

use super::mirrors::{self, host_of, MirrorFile};
use super::{fetch_properties_from, fetch_tile_from, FetchedTile, HipsSurvey, ServerBusy, TileFormat};

const DOWN_START: Duration = Duration::from_secs(30);
const DOWN_CAP: Duration = Duration::from_secs(600);
const BUSY_MIN: Duration = Duration::from_secs(5);
const BUSY_CAP: Duration = Duration::from_secs(300);
const BUSY_DEFAULT: Duration = Duration::from_secs(30);
/// A failed refresh is not retried sooner than this.
const REFRESH_RETRY: Duration = Duration::from_secs(3600);
const EWMA_ALPHA: f64 = 0.3;

#[derive(Debug, Clone)]
struct Endpoint {
    url: String,
    master: bool,
    in_flight: u32,
    ewma_ms: Option<f64>,
    failures: u32,
    down_until: Option<Instant>,
    /// The current down time is a busy answer, not an error.
    busy: bool,
}

impl Endpoint {
    fn new(url: String, master: bool) -> Self {
        Endpoint { url, master, in_flight: 0, ewma_ms: None, failures: 0, down_until: None, busy: false }
    }

    fn is_up(&self, now: Instant) -> bool {
        self.down_until.map_or(true, |t| now >= t)
    }
}

/// The endpoints of one survey, master first.
#[derive(Debug, Default)]
pub struct Pool {
    endpoints: Vec<Endpoint>,
}

/// What one request answered, as the pool records it.
enum Answer {
    Image(Vec<u8>),
    Absent,
    Busy(Option<Duration>),
    Error(anyhow::Error),
}

impl Pool {
    pub fn new(master: &str, mirrors: &[String]) -> Self {
        let mut endpoints = vec![Endpoint::new(master.to_string(), true)];
        endpoints.extend(mirrors.iter().filter(|m| m.as_str() != master).map(|m| Endpoint::new(m.clone(), false)));
        Pool { endpoints }
    }

    /// Replaces the mirrors, keeping what was learnt about endpoints that stay.
    fn set_mirrors(&mut self, mirrors: &[String]) {
        let old = std::mem::take(&mut self.endpoints);
        let master = old.iter().find(|e| e.master).cloned().expect("a pool always has its master");
        let mut endpoints = vec![master.clone()];
        for url in mirrors.iter().filter(|m| **m != master.url) {
            endpoints.push(old.iter().find(|e| e.url == *url).cloned().unwrap_or_else(|| Endpoint::new(url.clone(), false)));
        }
        self.endpoints = endpoints;
    }

    pub fn urls(&self) -> Vec<String> {
        self.endpoints.iter().map(|e| e.url.clone()).collect()
    }

    /// The next endpoint to ask, or `None` when every one not yet tried is
    /// down. After a mirror's 404 the master goes first.
    fn choose(&self, tried: &[usize], want_master: bool, now: Instant) -> Option<usize> {
        let up = |(i, e): &(usize, &Endpoint)| !tried.contains(i) && e.is_up(now);
        if want_master {
            if let Some((i, _)) = self.endpoints.iter().enumerate().filter(up).find(|(_, e)| e.master) {
                return Some(i);
            }
        }
        self.endpoints
            .iter()
            .enumerate()
            .filter(up)
            .min_by(|(_, a), (_, b)| {
                a.in_flight.cmp(&b.in_flight).then(a.ewma_ms.unwrap_or(0.0).total_cmp(&b.ewma_ms.unwrap_or(0.0)))
            })
            .map(|(i, _)| i)
    }

    fn record(&mut self, i: usize, answer: &Answer, took: Duration, now: Instant) {
        let e = &mut self.endpoints[i];
        e.in_flight = e.in_flight.saturating_sub(1);
        match answer {
            Answer::Image(_) | Answer::Absent => {
                let ms = took.as_secs_f64() * 1000.0;
                e.ewma_ms = Some(e.ewma_ms.map_or(ms, |m| m + EWMA_ALPHA * (ms - m)));
                e.failures = 0;
                e.down_until = None;
                e.busy = false;
            }
            Answer::Busy(retry_after) => {
                let wait = retry_after.map_or(BUSY_DEFAULT, |d| d.min(BUSY_CAP).max(BUSY_MIN));
                e.down_until = Some(now + wait);
                e.busy = true;
            }
            Answer::Error(_) => {
                e.failures += 1;
                let wait = DOWN_START.checked_mul(1 << (e.failures - 1).min(10)).map_or(DOWN_CAP, |d| d.min(DOWN_CAP));
                e.down_until = Some(now + wait);
                e.busy = false;
            }
        }
    }

    /// When every endpoint is down: the shortest busy wait left, if every
    /// down endpoint is down for being busy.
    fn all_busy(&self, now: Instant) -> Option<Duration> {
        if self.endpoints.iter().all(|e| e.busy && !e.is_up(now)) {
            self.endpoints.iter().filter_map(|e| e.down_until).map(|t| t.saturating_duration_since(now)).min()
        } else {
            None
        }
    }
}

/// One tile through `pool`, asking `get(url)` for each endpoint tried.
/// Returns the tile and the host that served it.
fn fetch_with(
    pool: &Mutex<Pool>,
    survey_id: &str,
    get: &dyn Fn(&str) -> Result<FetchedTile>,
) -> Result<(FetchedTile, String)> {
    let lock = || pool.lock().unwrap_or_else(|e| e.into_inner());
    let mut tried: Vec<usize> = Vec::new();
    let mut absent_from: Vec<usize> = Vec::new();
    let mut last_error: Option<anyhow::Error> = None;
    loop {
        let now = Instant::now();
        let (i, url) = {
            let mut p = lock();
            match p.choose(&tried, !absent_from.is_empty(), now) {
                Some(i) => {
                    p.endpoints[i].in_flight += 1;
                    (i, p.endpoints[i].url.clone())
                }
                None => break,
            }
        };
        tried.push(i);
        let started = Instant::now();
        let answer = match get(&url) {
            Ok(FetchedTile::Image(b)) => Answer::Image(b),
            Ok(FetchedTile::Absent) => Answer::Absent,
            Err(e) => match e.downcast_ref::<ServerBusy>() {
                Some(b) => Answer::Busy(b.retry_after),
                None => Answer::Error(e),
            },
        };
        let master = {
            let mut p = lock();
            p.record(i, &answer, started.elapsed(), Instant::now());
            p.endpoints[i].master
        };
        let host = host_of(&url).to_string();
        match answer {
            Answer::Image(bytes) => return Ok((FetchedTile::Image(bytes), host)),
            Answer::Absent if master => return Ok((FetchedTile::Absent, host)),
            Answer::Absent => absent_from.push(i),
            Answer::Busy(retry_after) => {
                tracing::debug!(survey_id, endpoint = %host, retry_after_s = retry_after.map(|d| d.as_secs()), "hips endpoint busy");
            }
            Answer::Error(e) => {
                let down_s = {
                    let p = lock();
                    p.endpoints[i].down_until.map_or(0, |t| t.saturating_duration_since(Instant::now()).as_secs())
                };
                tracing::warn!(survey_id, endpoint = %host, down_s, error = %format!("{e:#}"), "hips endpoint failed");
                last_error = Some(e);
            }
        }
    }

    let p = lock();
    let now = Instant::now();
    let mirrors: Vec<usize> = (0..p.endpoints.len()).filter(|&i| !p.endpoints[i].master).collect();
    if !mirrors.is_empty() && mirrors.iter().all(|i| absent_from.contains(i)) {
        // Every mirror answered 404 and the master could not answer. A busy
        // master may still have the tile: wait for it rather than record an
        // absence for good. A down master leaves the mirrors' word.
        if let Some(m) = p.endpoints.iter().find(|e| e.master && e.busy && !e.is_up(now)) {
            let wait = m.down_until.map_or(BUSY_DEFAULT, |t| t.saturating_duration_since(now));
            return Err(anyhow::Error::new(ServerBusy { retry_after: Some(wait.max(Duration::from_secs(1))) })
                .context(format!("the master of {survey_id} is busy and its mirrors lack the tile")));
        }
        return Ok((FetchedTile::Absent, String::new()));
    }
    if let Some(wait) = p.all_busy(now) {
        return Err(anyhow::Error::new(ServerBusy { retry_after: Some(wait.max(Duration::from_secs(1))) })
            .context(format!("every server of {survey_id} is busy")));
    }
    Err(last_error
        .map(|e| e.context(format!("no server of {survey_id} could serve the tile")))
        .unwrap_or_else(|| anyhow!("every server of {survey_id} is down for now")))
}

struct State {
    app_data: PathBuf,
    pools: HashMap<&'static str, Arc<Mutex<Pool>>>,
    file: Option<MirrorFile>,
    refreshing: bool,
    last_attempt: Option<Instant>,
}

fn state() -> &'static Mutex<Option<State>> {
    static S: OnceLock<Mutex<Option<State>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn with_state<T>(app_data: &Path, f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().map_or(true, |s| s.app_data != app_data) {
        let file = mirrors::read_file(app_data);
        *guard = Some(State { app_data: app_data.to_path_buf(), pools: HashMap::new(), file, refreshing: false, last_attempt: None });
    }
    f(guard.as_mut().expect("just set"))
}

fn mirrors_of(file: Option<&MirrorFile>, survey: &HipsSurvey) -> Vec<String> {
    file.and_then(|f| f.surveys.get(survey.id)).cloned().unwrap_or_default()
}

/// The survey's pool, created from the stored mirror list on first use. The
/// state lock is released before the caller touches the pool, so requests of
/// different tiles and surveys never wait on each other.
fn pool_of(app_data: &Path, survey: &'static HipsSurvey) -> Arc<Mutex<Pool>> {
    with_state(app_data, |s| {
        let mirrors = mirrors_of(s.file.as_ref(), survey);
        s.pools.entry(survey.id).or_insert_with(|| Arc::new(Mutex::new(Pool::new(survey.base_url, &mirrors)))).clone()
    })
}

/// The endpoints the survey is served from now, master first.
pub fn endpoints(app_data: &Path, survey: &'static HipsSurvey) -> Vec<String> {
    pool_of(app_data, survey).lock().unwrap_or_else(|e| e.into_inner()).urls()
}

fn install(app_data: &Path, file: MirrorFile) {
    with_state(app_data, |s| {
        for survey in super::SURVEYS {
            if let Some(pool) = s.pools.get(survey.id) {
                pool.lock().unwrap_or_else(|e| e.into_inner()).set_mirrors(&mirrors_of(Some(&file), survey));
            }
        }
        s.file = Some(file);
    });
}

/// Finds the mirrors again when the stored list is missing or a day old.
/// `blocking` runs the search on this thread (the pack); otherwise it runs on
/// a background thread and this call returns at once (browsing). At most one
/// search runs, and a failed one is not repeated within the hour.
pub fn refresh_if_stale(app_data: &Path, blocking: bool) {
    let due = with_state(app_data, |s| {
        let fresh = s.file.as_ref().is_some_and(|f| f.is_fresh(SystemTime::now()));
        let recent = s.last_attempt.is_some_and(|t| t.elapsed() < REFRESH_RETRY);
        if fresh || recent || s.refreshing {
            return false;
        }
        s.refreshing = true;
        s.last_attempt = Some(Instant::now());
        true
    });
    if !due {
        return;
    }
    let dir = app_data.to_path_buf();
    let run = move || {
        match mirrors::discover(&dir, &mirrors::Network, SystemTime::now()) {
            Ok(file) => {
                if let Err(e) = mirrors::write_file(&dir, &file) {
                    tracing::warn!(error = %format!("{e:#}"), "hips mirror list not saved");
                }
                install(&dir, file);
            }
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "hips mirror refresh failed"),
        }
        with_state(&dir, |s| s.refreshing = false);
    };
    if blocking {
        run();
    } else if let Err(e) = std::thread::Builder::new().name("hips-mirrors".into()).spawn(run) {
        tracing::warn!(error = %e, "hips mirror refresh thread failed to start");
        with_state(app_data, |s| s.refreshing = false);
    }
}

/// One tile from the survey's endpoints. Returns it with the serving host
/// (empty when every mirror answered 404 while the master was down).
pub fn fetch_tile(app_data: &Path, survey: &'static HipsSurvey, order: u8, npix: u64) -> Result<(FetchedTile, String)> {
    let format: TileFormat = survey.format;
    let pool = pool_of(app_data, survey);
    fetch_with(&pool, survey.id, &|url| fetch_tile_from(url, format, order, npix))
}

/// The survey's `properties`: from the master, else from a mirror.
pub fn fetch_properties(app_data: &Path, survey: &'static HipsSurvey) -> Result<String> {
    let urls = endpoints(app_data, survey);
    let mut last = None;
    for url in &urls {
        match fetch_properties_from(url, survey.id) {
            Ok(text) => return Ok(text),
            Err(e) => {
                tracing::warn!(survey_id = survey.id, endpoint = host_of(url), error = %format!("{e:#}"), "hips properties fetch failed");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("{} has no server", survey.id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::sync::Mutex as M;

    fn pool(mirrors: &[&str]) -> Mutex<Pool> {
        Mutex::new(Pool::new("https://master", &mirrors.iter().map(|s| s.to_string()).collect::<Vec<_>>()))
    }

    /// A fake network: per URL a list of answers, the last one repeating.
    struct Net(M<Map<String, Vec<Result<FetchedTile, Option<u64>>>>>, M<Vec<String>>);

    impl Net {
        fn new(script: &[(&str, &[Result<FetchedTile, Option<u64>>])]) -> Self {
            let m = script.iter().map(|(u, a)| (u.to_string(), a.to_vec())).collect();
            Net(M::new(m), M::new(Vec::new()))
        }

        /// `Err(Some(s))` is a busy answer with `Retry-After: s`, `Err(None)` a request error.
        fn get(&self, url: &str) -> Result<FetchedTile> {
            self.1.lock().unwrap().push(url.to_string());
            let mut m = self.0.lock().unwrap();
            let answers = m.get_mut(url).expect("unscripted URL");
            let a = if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() };
            match a {
                Ok(t) => Ok(t),
                Err(Some(s)) => Err(anyhow::Error::new(ServerBusy { retry_after: Some(Duration::from_secs(s)) })),
                Err(None) => Err(anyhow!("connection refused")),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.1.lock().unwrap().clone()
        }
    }

    fn img() -> FetchedTile {
        FetchedTile::Image(vec![1])
    }

    #[test]
    fn least_in_flight_then_fastest_and_new_endpoints_first() {
        let p = pool(&["https://m1", "https://m2"]).into_inner().unwrap();
        let now = Instant::now();
        let mut p = p;
        p.endpoints[0].ewma_ms = Some(50.0);
        p.endpoints[1].ewma_ms = Some(20.0);
        // m2 was never measured: it counts as the fastest.
        assert_eq!(p.choose(&[], false, now), Some(2));
        p.endpoints[2].ewma_ms = Some(90.0);
        assert_eq!(p.choose(&[], false, now), Some(1));
        p.endpoints[1].in_flight = 2;
        assert_eq!(p.choose(&[], false, now), Some(0), "fewest in flight wins over latency");
        assert_eq!(p.choose(&[0], false, now), Some(2));
        assert_eq!(p.choose(&[], true, now), Some(0), "after a mirror 404 the master goes first");
        p.endpoints[0].down_until = Some(now + Duration::from_secs(5));
        assert_eq!(p.choose(&[1, 2], false, now), None);
    }

    #[test]
    fn an_error_fails_over_and_marks_the_endpoint_down() {
        let pl = pool(&["https://m1"]);
        let net = Net::new(&[("https://master", &[Err(None)]), ("https://m1", &[Ok(img())])]);
        pl.lock().unwrap().endpoints[1].ewma_ms = Some(1000.0); // the master is asked first
        let (t, host) = fetch_with(&pl, "s", &|u| net.get(u)).unwrap();
        assert!(matches!(t, FetchedTile::Image(_)));
        assert_eq!(host, "m1");
        assert_eq!(net.asked(), vec!["https://master", "https://m1"]);
        let p = pl.lock().unwrap();
        assert!(!p.endpoints[0].is_up(Instant::now()));
        assert_eq!(p.endpoints[0].failures, 1);
        // Down time doubles per failure, capped.
        drop(p);
        let mut p = pl.lock().unwrap();
        for _ in 0..12 {
            p.endpoints[0].in_flight = 1;
            p.record(0, &Answer::Error(anyhow!("x")), Duration::ZERO, Instant::now());
        }
        let left = p.endpoints[0].down_until.unwrap().saturating_duration_since(Instant::now());
        assert!(left <= DOWN_CAP && left > DOWN_CAP - Duration::from_secs(5), "{left:?}");
    }

    #[test]
    fn a_mirror_404_asks_the_master_whose_404_is_absent() {
        let pl = pool(&["https://m1"]);
        pl.lock().unwrap().endpoints[0].ewma_ms = Some(1000.0); // the mirror is asked first
        let net = Net::new(&[("https://m1", &[Ok(FetchedTile::Absent)]), ("https://master", &[Ok(img())])]);
        let (t, host) = fetch_with(&pl, "s", &|u| net.get(u)).unwrap();
        assert!(matches!(t, FetchedTile::Image(_)), "a partial mirror's 404 is not the sky's");
        assert_eq!(host, "master");

        let net = Net::new(&[("https://m1", &[Ok(FetchedTile::Absent)]), ("https://master", &[Ok(FetchedTile::Absent)])]);
        assert!(matches!(fetch_with(&pl, "s", &|u| net.get(u)).unwrap().0, FetchedTile::Absent));
    }

    #[test]
    fn every_mirror_404_with_the_master_down_is_absent() {
        let pl = pool(&["https://m1", "https://m2"]);
        let net = Net::new(&[
            ("https://master", &[Err(None)]),
            ("https://m1", &[Ok(FetchedTile::Absent)]),
            ("https://m2", &[Ok(FetchedTile::Absent)]),
        ]);
        assert!(matches!(fetch_with(&pl, "s", &|u| net.get(u)).unwrap().0, FetchedTile::Absent));
        assert_eq!(net.asked(), vec!["https://master", "https://m1", "https://m2"]);
        // One mirror with the tile is enough.
        let pl = pool(&["https://m1", "https://m2"]);
        let net = Net::new(&[
            ("https://master", &[Err(None)]),
            ("https://m1", &[Ok(FetchedTile::Absent)]),
            ("https://m2", &[Ok(img())]),
        ]);
        assert!(matches!(fetch_with(&pl, "s", &|u| net.get(u)).unwrap().0, FetchedTile::Image(_)));
    }

    #[test]
    fn a_busy_master_with_mirrors_lacking_the_tile_is_waited_for() {
        let pl = pool(&["https://m1"]);
        let net = Net::new(&[("https://master", &[Err(Some(20))]), ("https://m1", &[Ok(FetchedTile::Absent)])]);
        let err = fetch_with(&pl, "s", &|u| net.get(u)).unwrap_err();
        let w = err.downcast_ref::<ServerBusy>().expect("busy").retry_after.unwrap();
        assert!(w > Duration::from_secs(15) && w <= Duration::from_secs(20), "{w:?}");
    }

    #[test]
    fn busy_everywhere_is_server_busy_with_the_shortest_wait() {
        let pl = pool(&["https://m1"]);
        let net = Net::new(&[("https://master", &[Err(Some(40))]), ("https://m1", &[Err(Some(1))])]);
        let err = fetch_with(&pl, "s", &|u| net.get(u)).unwrap_err();
        let busy = err.downcast_ref::<ServerBusy>().expect("busy");
        // m1's 1 s is floored at 5 s; 40 s for the master.
        let w = busy.retry_after.unwrap();
        assert!(w <= BUSY_MIN && w >= Duration::from_secs(4), "{w:?}");
        // One busy, one erroring: not a busy wait, a plain error.
        let pl = pool(&["https://m1"]);
        let net = Net::new(&[("https://master", &[Err(Some(40))]), ("https://m1", &[Err(None)])]);
        let err = fetch_with(&pl, "s", &|u| net.get(u)).unwrap_err();
        assert!(err.downcast_ref::<ServerBusy>().is_none(), "{err:#}");
    }

    #[test]
    fn a_master_only_pool_behaves_like_the_master() {
        let pl = pool(&[]);
        let net = Net::new(&[("https://master", &[Ok(FetchedTile::Absent)])]);
        assert!(matches!(fetch_with(&pl, "s", &|u| net.get(u)).unwrap().0, FetchedTile::Absent));
        let pl = pool(&[]);
        let net = Net::new(&[("https://master", &[Err(Some(7))])]);
        let err = fetch_with(&pl, "s", &|u| net.get(u)).unwrap_err();
        assert!(err.downcast_ref::<ServerBusy>().is_some());
    }

    #[test]
    fn new_mirrors_keep_what_was_learnt() {
        let pl = pool(&["https://m1"]);
        {
            let mut p = pl.lock().unwrap();
            p.endpoints[1].ewma_ms = Some(12.0);
            p.set_mirrors(&["https://m1".into(), "https://m2".into(), "https://master".into()]);
            assert_eq!(p.urls(), vec!["https://master", "https://m1", "https://m2"]);
            assert_eq!(p.endpoints[1].ewma_ms, Some(12.0));
            p.set_mirrors(&[]);
            assert_eq!(p.urls(), vec!["https://master"]);
        }
    }
}
