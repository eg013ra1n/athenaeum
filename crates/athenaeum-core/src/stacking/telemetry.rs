//! In-process stage telemetry for a stacking run (Task 13c Step 1b;
//! `docs/stacking/README.md` R-OSP-13). Timing only: nothing here touches a
//! pixel, a float or the order of any work.
//!
//! A [`StageMeter`] brackets one `StageTiming` interval. `start` logs
//! `stacking stage started` and spawns a 250 ms sampler thread; `finish`
//! (`outcome = "ok"`) or a drop without `finish` (`outcome = "aborted"`, an
//! `Err` / cancel path) stops AND joins the sampler, then logs
//! `stacking stage telemetry`. Joining first is load-bearing: a leaked
//! sampler would log samples carrying a stale `stage` into the next stage's
//! window.
//!
//! Field convention (the lab tools parse it): every `Option` is logged as
//! `-1` (`-1.0` for an `f64`), never omitted; `cores_busy` is an `f64` with
//! two decimals; bytes are `u64`, `*_ms` are `u64`. `swap_delta_bytes` is
//! `-1` when swap is unknown at either end (a real delta is a whole number
//! of pages, so `-1` cannot be a measured value).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::services::machine::{self, ProcSample};
use crate::stacking::plan::Stage;

/// Sampler interval.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
/// Every this-many intervals the sampler emits one debug `stacking telemetry sample`.
const SAMPLE_LOG_EVERY: u32 = 4;
/// An interval with fewer cores busy than this counts as serial.
const SERIAL_BELOW_CORES: f64 = 1.5;
/// An interval with at least this share of the logical CPUs busy counts as full.
const FULL_SHARE: f64 = 0.9;

/// The previous meter's finish per run, for `gap_ms`.
static LAST_FINISH: Mutex<Option<HashMap<i64, Instant>>> = Mutex::new(None);

/// What the sampler gathered over the stage.
#[derive(Debug, Clone, Copy)]
struct Agg {
    peak_footprint: u64,
    min_available: Option<u64>,
    serial_ns: u64,
    full_ns: u64,
}

struct Sampler {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: JoinHandle<Agg>,
}

struct Inner {
    run_id: i64,
    stage: &'static str,
    group_key: String,
    start: Instant,
    s0: ProcSample,
    gap_ms: u64,
    pressure0: Option<Pressure>,
    sampler: Option<Sampler>,
}

/// Brackets one stage interval; see the module docs.
pub(crate) struct StageMeter {
    inner: Option<Inner>,
}

impl StageMeter {
    /// Start a meter for `stage` of the run `rc` (a per-group interval passes its `group_key`).
    pub(crate) fn start(
        rc: &crate::stacking::run::RunContext,
        stage: Stage,
        group_key: Option<&str>,
    ) -> Self {
        Self::start_raw(rc.run_id, stage.as_str(), group_key)
    }

    pub(crate) fn start_raw(run_id: i64, stage: &'static str, group_key: Option<&str>) -> Self {
        let now = Instant::now();
        let gap_ms = LAST_FINISH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(&run_id))
            .map_or(0, |t| now.saturating_duration_since(*t).as_millis() as u64);
        let group_key = group_key.unwrap_or("").to_string();
        // Free pages go back to the OS BEFORE the stage's baseline is read.
        match machine::release_free_memory() {
            Some((before, after)) => tracing::debug!(
                run_id,
                stage,
                group_key = %group_key,
                footprint_before_bytes = before,
                footprint_after_bytes = after,
                released_bytes = before.saturating_sub(after),
                "memory relief"
            ),
            None => tracing::debug!(
                run_id,
                stage,
                group_key = %group_key,
                footprint_before_bytes = -1i64,
                footprint_after_bytes = -1i64,
                released_bytes = -1i64,
                "memory relief"
            ),
        }
        vmmap_diag(run_id, stage, &group_key);
        let s0 = machine::proc_sample();
        tracing::info!(
            run_id,
            stage,
            group_key = %group_key,
            footprint_bytes = s0.footprint,
            "stacking stage started"
        );
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let sampler = {
            let stop = stop.clone();
            let group_key = group_key.clone();
            // The sampler logs through the starter's dispatcher: a thread spawned here would
            // otherwise see only the global default (and a scoped test capture nothing).
            let dispatch = tracing::dispatcher::get_default(|d| d.clone());
            std::thread::Builder::new()
                .name("stacking-sampler".to_string())
                .spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        sampler_loop(run_id, stage, &group_key, s0, &stop)
                    })
                })
        };
        let sampler = match sampler {
            Ok(handle) => Some(Sampler { stop, handle }),
            Err(e) => {
                tracing::warn!(run_id, stage, error = %e, "stacking sampler thread not started; stage telemetry has no peak / serial figures");
                None
            }
        };
        Self {
            inner: Some(Inner {
                run_id,
                stage,
                group_key,
                start: now,
                s0,
                gap_ms,
                pressure0: read_pressure(),
                sampler,
            }),
        }
    }

    /// End the interval normally: `stacking stage telemetry` with `outcome = "ok"`.
    pub(crate) fn finish(mut self) {
        if let Some(inner) = self.inner.take() {
            inner.conclude("ok");
        }
    }
}

impl Drop for StageMeter {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.conclude("aborted");
        }
    }
}

/// One `memory checkpoint` debug: this process's footprint at a named point
/// of a stage (Task 13c 4a-0). Logging only.
pub(crate) fn memory_checkpoint(run_id: i64, stage: &str, group_key: &str, point: &str) {
    tracing::debug!(
        run_id,
        stage,
        group_key = %group_key,
        point,
        footprint_bytes = machine::proc_sample().footprint,
        "memory checkpoint"
    );
}

/// Dev-only (`ATH_VMMAP_DIAG=1`, macOS): `vmmap --summary <own pid>` into
/// `<log dir>/vmmap/<run_id>-<stage>-<group>.txt`. A failure warns and the
/// stage carries on.
fn vmmap_diag(run_id: i64, stage: &str, group_key: &str) {
    if !cfg!(target_os = "macos") || std::env::var("ATH_VMMAP_DIAG").as_deref() != Ok("1") {
        return;
    }
    let Some(dir) = crate::logging::get_path() else {
        tracing::warn!(run_id, stage, "vmmap diagnostic skipped: no log directory");
        return;
    };
    let dir = dir.join("vmmap");
    let group: String = group_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{run_id}-{stage}-{group}.txt"));
    let result = std::fs::create_dir_all(&dir).and_then(|()| {
        let out = std::process::Command::new("vmmap")
            .arg("--summary")
            .arg(std::process::id().to_string())
            .output()?;
        std::fs::write(&path, &out.stdout)
    });
    if let Err(e) = result {
        tracing::warn!(run_id, stage, path = %path.display(), error = %e, "vmmap diagnostic failed; the stage carries on");
    }
}

fn sampler_loop(
    run_id: i64,
    stage: &'static str,
    group_key: &str,
    first: ProcSample,
    stop: &(Mutex<bool>, Condvar),
) -> Agg {
    let logical = machine::profile().logical_cpus.max(1) as f64;
    let mut agg = Agg {
        peak_footprint: first.footprint,
        min_available: None,
        serial_ns: 0,
        full_ns: 0,
    };
    let mut prev = first;
    let mut prev_at = Instant::now();
    let mut intervals = 0u32;
    let (flag, cv) = stop;
    loop {
        let deadline = prev_at + SAMPLE_INTERVAL;
        let mut stopped = flag.lock().unwrap_or_else(|e| e.into_inner());
        while !*stopped {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (g, _) = cv
                .wait_timeout(stopped, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            stopped = g;
        }
        let done = *stopped;
        drop(stopped);

        let now = Instant::now();
        let sample = machine::proc_sample();
        let wall_ns = now.saturating_duration_since(prev_at).as_nanos() as u64;
        if wall_ns > 0 {
            let busy = sample.cpu_ns.saturating_sub(prev.cpu_ns) as f64 / wall_ns as f64;
            let p_busy = match (sample.p_cpu_ns, prev.p_cpu_ns) {
                (Some(a), Some(b)) => a.saturating_sub(b) as f64 / wall_ns as f64,
                _ => -1.0,
            };
            if busy < SERIAL_BELOW_CORES {
                agg.serial_ns += wall_ns;
            }
            if busy >= FULL_SHARE * logical {
                agg.full_ns += wall_ns;
            }
            agg.peak_footprint = agg.peak_footprint.max(sample.footprint);
            let available = machine::available_memory();
            if let Some(a) = available {
                agg.min_available = Some(agg.min_available.map_or(a, |m| m.min(a)));
            }
            intervals += 1;
            if !done && intervals % SAMPLE_LOG_EVERY == 0 {
                #[cfg(test)]
                DEBUG_SAMPLES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(run_id);
                tracing::debug!(
                    run_id,
                    stage,
                    group_key = %group_key,
                    cores_busy = round2(busy),
                    p_cores_busy = if p_busy < 0.0 { -1.0 } else { round2(p_busy) },
                    footprint_bytes = sample.footprint,
                    available_bytes = machine::opt_i64(available),
                    "stacking telemetry sample"
                );
            }
        }
        prev = sample;
        prev_at = now;
        if done {
            return agg;
        }
    }
}

impl Inner {
    fn conclude(mut self, outcome: &'static str) {
        // Stop AND join the sampler before anything is logged.
        let agg = match self.sampler.take() {
            Some(s) => {
                {
                    let (flag, cv) = &*s.stop;
                    *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    cv.notify_all();
                }
                match s.handle.join() {
                    Ok(a) => a,
                    Err(_) => {
                        tracing::warn!(run_id = self.run_id, stage = self.stage, "stacking sampler thread panicked; stage telemetry has no peak / serial figures");
                        Agg {
                            peak_footprint: 0,
                            min_available: None,
                            serial_ns: 0,
                            full_ns: 0,
                        }
                    }
                }
            }
            None => Agg {
                peak_footprint: 0,
                min_available: None,
                serial_ns: 0,
                full_ns: 0,
            },
        };
        let end = Instant::now();
        let s1 = machine::proc_sample();
        let pressure1 = read_pressure();
        let wall = end.saturating_duration_since(self.start);
        let duration_ms = wall.as_millis() as u64;
        let cpu_ns = s1.cpu_ns.saturating_sub(self.s0.cpu_ns);
        let cores_busy = if wall.as_nanos() == 0 {
            0.0
        } else {
            round2(cpu_ns as f64 / wall.as_nanos() as f64)
        };
        let p_cpu_ms: i64 = match (s1.p_cpu_ns, self.s0.p_cpu_ns) {
            (Some(a), Some(b)) => (a.saturating_sub(b) / 1_000_000) as i64,
            _ => -1,
        };
        let runnable_ms: i64 = match (s1.runnable_ns, self.s0.runnable_ns) {
            (Some(a), Some(b)) => (a.saturating_sub(b) / 1_000_000) as i64,
            _ => -1,
        };
        let swap_delta_bytes: i64 = match (s1.swap_used, self.s0.swap_used) {
            (Some(a), Some(b)) => a as i64 - b as i64,
            _ => -1,
        };
        let peak_footprint = agg.peak_footprint.max(s1.footprint).max(self.s0.footprint);
        let pressure = match (self.pressure0, pressure1) {
            (Some(a), Some(b)) => Some(b.delta_since(a)),
            _ => None,
        };

        let run_id = self.run_id;
        let stage = self.stage;
        let group_key = std::mem::take(&mut self.group_key);
        let gap_ms = self.gap_ms;
        let disk_read_bytes = s1.disk_read.saturating_sub(self.s0.disk_read);
        let disk_write_bytes = s1.disk_write.saturating_sub(self.s0.disk_write);
        let lifetime_max_footprint_bytes = s1.footprint_lifetime_max;
        let cpu_ms = cpu_ns / 1_000_000;
        let serial_ms = agg.serial_ns / 1_000_000;
        let full_ms = agg.full_ns / 1_000_000;
        let peak_footprint_bytes = peak_footprint;
        let min_available_bytes = machine::opt_i64(agg.min_available);

        // One call site for the shared fields; Linux adds the pressure deltas.
        macro_rules! telemetry_line {
            ($($extra:tt)*) => {
                tracing::info!(
                    run_id,
                    stage,
                    group_key = %group_key,
                    outcome,
                    duration_ms,
                    cpu_ms,
                    p_cpu_ms,
                    runnable_ms,
                    cores_busy,
                    serial_ms,
                    full_ms,
                    gap_ms,
                    peak_footprint_bytes,
                    lifetime_max_footprint_bytes,
                    min_available_bytes,
                    swap_delta_bytes,
                    disk_read_bytes,
                    disk_write_bytes,
                    $($extra)*
                    "stacking stage telemetry"
                )
            };
        }
        match pressure {
            Some(p) => telemetry_line!(
                cpu_pressure_us = p.cpu_us,
                io_pressure_us = p.io_us,
                mem_pressure_us = p.mem_us,
            ),
            // Linux has the files: an unreadable one is -1, never an absent field.
            None if cfg!(target_os = "linux") => telemetry_line!(
                cpu_pressure_us = -1i64,
                io_pressure_us = -1i64,
                mem_pressure_us = -1i64,
            ),
            None => telemetry_line!(),
        }

        let mut last = LAST_FINISH.lock().unwrap_or_else(|e| e.into_inner());
        let map = last.get_or_insert_with(HashMap::new);
        if map.len() > 256 {
            map.clear();
        }
        map.insert(self.run_id, Instant::now());
    }
}

/// Run ids of the debug samples emitted so far (tests wait on this, not on the clock).
#[cfg(test)]
static DEBUG_SAMPLES: Mutex<Vec<i64>> = Mutex::new(Vec::new());

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Linux cgroup v2 pressure (PSI) totals
// ---------------------------------------------------------------------------

/// The `some total=` counters (microseconds) of the cgroup's `cpu` / `io` /
/// `memory` pressure files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pressure {
    cpu_us: u64,
    io_us: u64,
    mem_us: u64,
}

impl Pressure {
    fn delta_since(self, before: Pressure) -> Pressure {
        Pressure {
            cpu_us: self.cpu_us.saturating_sub(before.cpu_us),
            io_us: self.io_us.saturating_sub(before.io_us),
            mem_us: self.mem_us.saturating_sub(before.mem_us),
        }
    }
}

/// `total=` of the `some` line of a PSI file:
/// `some avg10=0.00 avg60=0.00 avg300=0.00 total=12345`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_pressure_some_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.trim_start().starts_with("some "))?;
    line.split_whitespace()
        .find_map(|tok| tok.strip_prefix("total="))?
        .parse()
        .ok()
}

/// The cgroup v2 path of a `/proc/self/cgroup` body: the `0::/path` entry.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn cgroup_v2_path(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix("0::"))
}

#[cfg(target_os = "linux")]
fn read_pressure() -> Option<Pressure> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok();
    let rel = own
        .as_deref()
        .and_then(cgroup_v2_path)
        .map(|p| p.trim().trim_start_matches('/').to_string())
        .unwrap_or_default();
    let read = |name: &str| -> Option<u64> {
        let nested = std::path::Path::new("/sys/fs/cgroup")
            .join(&rel)
            .join(format!("{name}.pressure"));
        let text = std::fs::read_to_string(&nested)
            .or_else(|_| std::fs::read_to_string(format!("/sys/fs/cgroup/{name}.pressure")))
            .ok()?;
        parse_pressure_some_total(&text)
    };
    Some(Pressure {
        cpu_us: read("cpu")?,
        io_us: read("io")?,
        mem_us: read("memory")?,
    })
}

#[cfg(not(target_os = "linux"))]
fn read_pressure() -> Option<Pressure> {
    None
}

/// A scoped log capture shared by the telemetry and fan-out tests.
#[cfg(test)]
pub(crate) fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }
    // tracing-core shortcuts interest to the registering thread's default while only ONE
    // dispatcher exists; a second, permanent no-op dispatcher turns that off for the binary.
    static SECOND_DISPATCH: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SECOND_DISPATCH
        .get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let buf = Buf(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.0.lock().unwrap().clone();
    (out, String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_with<'a>(text: &'a str, needle: &str) -> Vec<&'a str> {
        text.lines().filter(|l| l.contains(needle)).collect()
    }

    #[test]
    fn pressure_parser_reads_the_some_total() {
        let psi = "some avg10=0.50 avg60=0.10 avg300=0.02 total=123456\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=99\n";
        assert_eq!(parse_pressure_some_total(psi), Some(123_456));
        assert_eq!(parse_pressure_some_total("full avg10=0 total=5\n"), None);
        assert_eq!(
            parse_pressure_some_total("some avg10=0.00 total=abc\n"),
            None
        );
        assert_eq!(parse_pressure_some_total(""), None);
        assert_eq!(
            cgroup_v2_path("12:cpu:/x\n0::/user.slice/app.scope\n"),
            Some("/user.slice/app.scope")
        );
        assert_eq!(cgroup_v2_path("1:name=systemd:/x\n"), None);
    }

    #[test]
    fn start_logs_memory_relief_before_stage_started() {
        let ((), text) = capture_logs(|| {
            let m = StageMeter::start_raw(9_002, "measure", Some("g2"));
            m.finish();
        });
        let relief = text.find("memory relief").expect("memory relief line");
        let started = text.find("stacking stage started").expect("started line");
        assert!(
            relief < started,
            "relief must precede the started line:\n{text}"
        );
        let line = text.lines().find(|l| l.contains("memory relief")).unwrap();
        for f in [
            "run_id=9002",
            "stage=\"measure\"",
            "footprint_before_bytes=",
            "footprint_after_bytes=",
            "released_bytes=",
        ] {
            assert!(line.contains(f), "missing {f} in {line}");
        }
    }

    #[test]
    fn memory_checkpoint_carries_its_fields() {
        let ((), text) =
            capture_logs(|| memory_checkpoint(9_003, "normalize", "g3", "ln reference built"));
        let line = text
            .lines()
            .find(|l| l.contains("memory checkpoint"))
            .expect("line");
        for f in [
            "run_id=9003",
            "point=\"ln reference built\"",
            "footprint_bytes=",
        ] {
            assert!(line.contains(f), "missing {f} in {line}");
        }
    }

    #[test]
    fn finished_meter_logs_started_and_ok_telemetry() {
        let ((), text) = capture_logs(|| {
            let m = StageMeter::start_raw(9_001, "measure", Some("g1"));
            // Burn a little CPU and let the sampler tick at least once.
            let t = Instant::now();
            let mut x = 0u64;
            while t.elapsed() < Duration::from_millis(300) {
                x = x.wrapping_add(1);
            }
            std::hint::black_box(x);
            m.finish();
        });
        let started = lines_with(&text, "stacking stage started");
        assert_eq!(started.len(), 1, "{text}");
        assert!(started[0].contains("run_id=9001") && started[0].contains("footprint_bytes="));
        let tele = lines_with(&text, "stacking stage telemetry");
        assert_eq!(tele.len(), 1, "{text}");
        for field in [
            "run_id=9001",
            "stage=\"measure\"",
            "group_key=g1",
            "outcome=\"ok\"",
            "duration_ms=",
            "cpu_ms=",
            "p_cpu_ms=",
            "runnable_ms=",
            "cores_busy=",
            "serial_ms=",
            "full_ms=",
            "gap_ms=",
            "peak_footprint_bytes=",
            "lifetime_max_footprint_bytes=",
            "min_available_bytes=",
            "swap_delta_bytes=",
            "disk_read_bytes=",
            "disk_write_bytes=",
        ] {
            assert!(tele[0].contains(field), "{field} missing in {}", tele[0]);
        }
    }

    #[test]
    fn dropped_meter_logs_aborted_and_no_sample_after_it() {
        let ((), text) = capture_logs(|| {
            {
                let _m = StageMeter::start_raw(9_002, "register", None);
                // Keep the meter alive until a debug sample was emitted (no clock slack);
                // dropping it then = aborted.
                let deadline = Instant::now() + Duration::from_secs(20);
                while !DEBUG_SAMPLES
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&9_002)
                {
                    assert!(Instant::now() < deadline, "no debug sample in 20 s");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            // A leaked sampler would keep sampling here.
            std::thread::sleep(Duration::from_millis(1_100));
        });
        let aborted = text
            .lines()
            .position(|l| {
                l.contains("stacking stage telemetry") && l.contains("outcome=\"aborted\"")
            })
            .unwrap_or_else(|| panic!("no aborted line: {text}"));
        let samples: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("stacking telemetry sample") && l.contains("run_id=9002"))
            .map(|(i, _)| i)
            .collect();
        assert!(!samples.is_empty(), "the 1 s sample was due: {text}");
        assert!(
            samples.iter().all(|&i| i < aborted),
            "a sample after the aborted line: {text}"
        );
    }

    #[test]
    fn gap_is_the_time_since_the_previous_meter_of_the_same_run() {
        let ((), text) = capture_logs(|| {
            StageMeter::start_raw(9_003, "a", None).finish();
            std::thread::sleep(Duration::from_millis(120));
            StageMeter::start_raw(9_003, "b", None).finish();
        });
        let tele = lines_with(&text, "stacking stage telemetry");
        assert_eq!(tele.len(), 2, "{text}");
        assert!(tele[0].contains("gap_ms=0"), "{}", tele[0]);
        let gap: u64 = tele[1]
            .split("gap_ms=")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert!((100..2_000).contains(&gap), "gap {gap} in {}", tele[1]);
    }
}
