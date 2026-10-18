//! What this machine (or the container / job it runs in) actually has, probed
//! once, plus the live reads the stacking pipeline sizes itself from
//! (Task 13c; `docs/stacking/README.md` R-OSP-13).
//!
//! Every parser is a pure function on a `&str` so it is unit-tested on every
//! platform; only the file / syscall read is `cfg`-gated. An unknown value is
//! `None` (or `0` for a counter) — never a guess.

use std::sync::OnceLock;

/// The static facts about the machine. `Option` fields are `None` when the
/// platform cannot say (non-hybrid x86 has no P / E split; no cgroup quota).
#[derive(Debug, Clone, PartialEq)]
pub struct MachineProfile {
    pub logical_cpus: usize,
    pub perf_cores: Option<usize>,
    /// `Some(0)` on a hybrid-capable platform that reports no efficiency level.
    pub eff_cores: Option<usize>,
    /// CPU quota in cores (cgroup `cpu.max`, Windows job hard cap).
    pub cpu_quota: Option<f64>,
    /// Physical memory.
    pub mem_total: Option<u64>,
    /// cgroup / job memory limit, only when it is below physical memory.
    pub mem_limit: Option<u64>,
}

impl MachineProfile {
    /// Memory this process may actually use: the limit when set, else physical.
    pub fn effective_mem(&self) -> Option<u64> {
        match (self.mem_total, self.mem_limit) {
            (Some(t), Some(l)) => Some(t.min(l)),
            (Some(t), None) => Some(t),
            (None, l) => l,
        }
    }
}

static PROFILE: OnceLock<MachineProfile> = OnceLock::new();

/// The machine profile, probed on first use.
pub fn profile() -> &'static MachineProfile {
    PROFILE.get_or_init(probe_profile)
}

/// Physical RAM this process may actually use, in bytes — the old
/// `integration::band_budget::total_ram_bytes` (re-exported there).
///
/// On Linux this is `min(MemTotal, container limit)` — **load-bearing for the
/// Docker/web build**: `/proc/meminfo` reports the HOST's RAM inside a
/// container, so without the cgroup read a 2 GB container would size an 8 GiB
/// budget and be OOM-killed.
pub fn total_ram_bytes() -> Option<u64> {
    profile().effective_mem()
}

fn probe_profile() -> MachineProfile {
    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "available_parallelism failed; assuming 4 logical cpus");
            4
        });
    let (perf_cores, eff_cores) = platform_core_split();
    let cpu_quota = platform_cpu_quota(logical_cpus);
    let (mem_total, mem_limit) = platform_memory();
    let mem_limit = match (mem_total, mem_limit) {
        (Some(t), Some(l)) if l < t => Some(l),
        (None, Some(l)) => Some(l),
        _ => None,
    };
    MachineProfile {
        logical_cpus,
        perf_cores,
        eff_cores,
        cpu_quota,
        mem_total,
        mem_limit,
    }
}

// ---------------------------------------------------------------------------
// Pure parsers
// ---------------------------------------------------------------------------

/// cgroup v2 `cpu.max`: `max 100000` -> no quota, `200000 100000` -> 2.0.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_cpu_max(text: &str) -> Option<f64> {
    let mut it = text.split_whitespace();
    let quota = it.next()?;
    let period = it.next()?;
    if quota == "max" {
        return None;
    }
    quota_over_period(quota.parse().ok()?, period.parse().ok()?)
}

/// cgroup v1 `cpu.cfs_quota_us` / `cpu.cfs_period_us` (`-1` = no quota).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_cfs_pair(quota: &str, period: &str) -> Option<f64> {
    quota_over_period(quota.trim().parse().ok()?, period.trim().parse().ok()?)
}

fn quota_over_period(quota: i64, period: i64) -> Option<f64> {
    if quota <= 0 || period <= 0 {
        return None;
    }
    Some(quota as f64 / period as f64)
}

/// A kernel cpu list (`0-7,16`) -> the number of cpus it names.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_cpu_list(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let mut n = 0usize;
    for part in text.split(',') {
        let part = part.trim();
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (usize, usize) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                if b < a {
                    return None;
                }
                n += b - a + 1;
            }
            None => {
                part.parse::<usize>().ok()?;
                n += 1;
            }
        }
    }
    Some(n)
}

/// A kernel cpu list (`0-7,16`) -> the cpu ids it names.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn expand_cpu_list(text: &str) -> Option<Vec<usize>> {
    let text = text.trim();
    let mut out = Vec::new();
    if text.is_empty() {
        return Some(out);
    }
    for part in text.split(',') {
        let part = part.trim();
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (usize, usize) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                if b < a {
                    return None;
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse::<usize>().ok()?),
        }
    }
    Some(out)
}

/// Distinct physical cores among `cpus`: `ids(cpu)` is that cpu's
/// `(package id, core id)`; a cpu whose ids are unreadable counts as its own
/// core (never fewer than the truth the topology files could not give).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn count_distinct_cores(
    cpus: &[usize],
    ids: impl Fn(usize) -> Option<(i64, i64)>,
) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut unknown = 0usize;
    for &c in cpus {
        match ids(c) {
            Some(id) => {
                seen.insert(id);
            }
            None => unknown += 1,
        }
    }
    seen.len() + unknown
}

/// One `key 123` line of a cgroup `memory.stat`, in bytes.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_stat_field(text: &str, key: &str) -> Option<u64> {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some(key) {
            return it.next()?.parse().ok();
        }
    }
    None
}

/// Memory a cgroup can still use: `limit - (current - reclaimable page cache)`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn cgroup_available(limit: u64, current: u64, inactive_file: u64) -> u64 {
    limit.saturating_sub(current.saturating_sub(inactive_file))
}

/// Log a warning at most once per `flag` (a per-sample failure must not flood).
fn warn_once(flag: &std::sync::atomic::AtomicBool, f: impl FnOnce()) {
    if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
        f();
    }
}

/// One `Key:   123 kB` line of `/proc/meminfo` or `/proc/self/status`, in bytes.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_kb_field(text: &str, key: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// `/proc/self/io` -> (read_bytes, write_bytes).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_proc_io(text: &str) -> Option<(u64, u64)> {
    let field = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|r| r.trim().parse().ok())
    };
    Some((field("read_bytes:")?, field("write_bytes:")?))
}

/// Parse one cgroup memory-limit file. `None` means "no limit here": cgroup v2
/// writes the literal `max`, and v1 writes a sentinel near `u64::MAX` — a
/// number that large is not a container limit, it is the absence of one.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) fn parse_cgroup_limit(text: &str) -> Option<u64> {
    const UNLIMITED_FLOOR: u64 = 1 << 62;
    let v: u64 = text.trim().parse().ok()?;
    if v == 0 || v >= UNLIMITED_FLOOR {
        None
    } else {
        Some(v)
    }
}

/// Pure computation over `/proc/meminfo`'s content plus the two possible
/// cgroup limit-file contents: `min(MemTotal, container limit)`. cgroup v2
/// wins when it names a real limit; `max` (or absent) falls through to v1.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn linux_total_ram_from(
    meminfo: &str,
    v2: Option<&str>,
    v1: Option<&str>,
) -> Option<u64> {
    let total = parse_kb_field(meminfo, "MemTotal:")?;
    Some(match cgroup_limit(v2, v1) {
        Some(l) => total.min(l),
        None => total,
    })
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn cgroup_limit(v2: Option<&str>, v1: Option<&str>) -> Option<u64> {
    v2.and_then(parse_cgroup_limit)
        .or_else(|| v1.and_then(parse_cgroup_limit))
}

/// Efficiency classes `(class, physical cores)` -> `(perf, eff)`. One class
/// (non-hybrid) says nothing; with several, the highest class is the P group.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn split_efficiency_classes(classes: &[(u8, usize)]) -> (Option<usize>, Option<usize>) {
    let Some(top) = classes.iter().map(|c| c.0).max() else {
        return (None, None);
    };
    if classes.iter().all(|c| c.0 == top) {
        return (None, None);
    }
    let perf = classes.iter().filter(|c| c.0 == top).map(|c| c.1).sum();
    let eff = classes.iter().filter(|c| c.0 != top).map(|c| c.1).sum();
    (Some(perf), Some(eff))
}

// ---------------------------------------------------------------------------
// Pool width and memory budget
// ---------------------------------------------------------------------------

/// The `image_pool` width and why: `RAYON_NUM_THREADS` (a positive integer)
/// -> `"env"`; else `logical_cpus` capped by `ceil(cpu_quota)` -> `"cgroup"` /
/// `"job"` ONLY when the quota binds (`ceil(quota) < logical`); else `"auto"`.
/// No fixed ceiling. On Linux `available_parallelism` already applies the
/// cgroup quota, so `logical_cpus` is itself quota-aware there and a quota that
/// does not bind changes nothing.
///
/// Resolved ONCE per process (the host sizes `image_pool` from it at startup):
/// later callers get the same answer without re-reading the environment or
/// repeating its warning.
pub fn pool_threads(p: &MachineProfile) -> (usize, &'static str) {
    static RESOLVED: OnceLock<(usize, &'static str)> = OnceLock::new();
    *RESOLVED
        .get_or_init(|| pool_threads_with(p, std::env::var("RAYON_NUM_THREADS").ok().as_deref()))
}

pub(crate) fn pool_threads_with(p: &MachineProfile, env: Option<&str>) -> (usize, &'static str) {
    if let Some(v) = env {
        match v.trim().parse::<usize>() {
            Ok(n) if n > 0 => return (n, "env"),
            _ => {
                tracing::warn!(value = %v, "RAYON_NUM_THREADS is not a positive integer; sizing the pool automatically")
            }
        }
    }
    let logical = p.logical_cpus.max(1);
    match p.cpu_quota {
        Some(q) if q > 0.0 && (q.ceil() as usize).max(1) < logical => {
            let why = if cfg!(windows) { "job" } else { "cgroup" };
            ((q.ceil() as usize).max(1), why)
        }
        _ => (logical, "auto"),
    }
}

/// The stacking memory budget for a configured `compute.memory_budget_mb`:
/// `> 0` -> that many MiB (`"setting"`); `0` -> a quarter of the memory this
/// process may use (`"auto"`). `None` when auto and the total is unknown
/// (admission 1, as before). The setting is a CAP on memory, not a promise: a
/// value above the effective memory (`mem_limit`, else `mem_total`) is clamped
/// to it (`"setting-clamped"`, one `warn!` per distinct configured value); `1`
/// MiB stays valid (the admission-1 arm).
pub fn memory_budget_from(configured_mb: u64, p: &MachineProfile) -> Option<(u64, &'static str)> {
    if configured_mb > 0 {
        let bytes = configured_mb.saturating_mul(1024 * 1024);
        if let Some(cap) = p.effective_mem() {
            if bytes > cap {
                if CLAMP_WARNED_MB.swap(configured_mb, std::sync::atomic::Ordering::Relaxed)
                    != configured_mb
                {
                    tracing::warn!(
                        configured_mb,
                        effective_mem_bytes = cap,
                        "compute.memory_budget_mb above the memory this process may use; clamped"
                    );
                }
                return Some((cap, "setting-clamped"));
            }
        }
        return Some((bytes, "setting"));
    }
    p.effective_mem().map(|t| (t / 4, "auto"))
}

/// The configured value last warned about as clamped (a warn per change, not per call).
static CLAMP_WARNED_MB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`memory_budget_from`] with the live setting read from the catalog, or an
/// `Err` naming why the setting could not be read (the admission gate keeps its
/// current target on `Err`; it never guesses).
pub fn try_memory_budget(
    ctx: &super::ServiceContext,
) -> Result<Option<(u64, &'static str)>, String> {
    let Some(db) = ctx.db.get() else {
        return Err("database not initialised".to_string());
    };
    // try_conn, not conn: an exhausted pool is an `Err` here (the gate keeps its target), never a panic.
    let conn = db.try_conn().map_err(|e| e.to_string())?;
    let mb = ctx
        .settings
        .get_compute_memory_budget_mb(&conn)
        .map_err(|e| e.to_string())?;
    Ok(memory_budget_from(mb, profile()))
}

/// [`memory_budget_from`] with the live setting read from the catalog. An
/// unreadable setting is logged and treated as auto.
pub fn memory_budget(ctx: &super::ServiceContext) -> Option<(u64, &'static str)> {
    // Called by the admission gate and every `fan_budget`: warn once per
    // cause, not once per call.
    static UNREADABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static NO_DB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static NO_CONN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let configured = match ctx.db.get() {
        // try_conn, not conn: an exhausted pool degrades to auto, never panics.
        Some(db) => match db.try_conn() {
            Ok(conn) => match ctx.settings.get_compute_memory_budget_mb(&conn) {
                Ok(mb) => mb,
                Err(e) => {
                    warn_once(&UNREADABLE, || {
                        tracing::warn!(error = %e, "compute.memory_budget_mb unreadable; using auto")
                    });
                    0
                }
            },
            Err(e) => {
                warn_once(&NO_CONN, || {
                    tracing::warn!(error = %e, "no database connection for compute.memory_budget_mb; using auto")
                });
                0
            }
        },
        None => {
            warn_once(&NO_DB, || {
                tracing::warn!("database not initialised; compute.memory_budget_mb read as auto")
            });
            0
        }
    };
    memory_budget_from(configured, profile())
}

/// Optional integer logged as `-1` when unknown (the lab tools parse `-1`).
pub(crate) fn opt_i64(v: Option<u64>) -> i64 {
    v.map_or(-1, |n| n as i64)
}

/// The profile fields every `info!` line carries (`logical_cpus`, `perf_cores`,
/// `eff_cores`, `cpu_quota`, `mem_total_bytes`, `mem_limit_bytes`,
/// `mem_available_bytes`; unknown = `-1`) — one macro so the names live once.
macro_rules! machine_fields_info {
    ($msg:literal $(, $($extra:tt)*)?) => {{
        let p = $crate::services::machine::profile();
        tracing::info!(
            logical_cpus = p.logical_cpus,
            perf_cores = $crate::services::machine::opt_i64(p.perf_cores.map(|v| v as u64)),
            eff_cores = $crate::services::machine::opt_i64(p.eff_cores.map(|v| v as u64)),
            cpu_quota = p.cpu_quota.unwrap_or(-1.0),
            mem_total_bytes = $crate::services::machine::opt_i64(p.mem_total),
            mem_limit_bytes = $crate::services::machine::opt_i64(p.mem_limit),
            mem_available_bytes = $crate::services::machine::opt_i64($crate::services::machine::available_memory()),
            $($($extra)*,)?
            $msg
        );
    }};
}
#[allow(unused_imports)] // only the render-gated stacking run uses it
pub(crate) use machine_fields_info;

/// The one `compute budget resolved` line, logged by both hosts at start.
pub fn log_compute_budget_resolved(pool_threads: usize, pool_why: &'static str) {
    machine_fields_info!("compute budget resolved", pool_threads, pool_why);
}

// ---------------------------------------------------------------------------
// Process sample
// ---------------------------------------------------------------------------

/// One reading of this process's own resource counters. Unknown -> `None` / 0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProcSample {
    /// user + system CPU time, nanoseconds.
    pub cpu_ns: u64,
    /// CPU time on performance cores (macOS only), nanoseconds.
    pub p_cpu_ns: Option<u64>,
    /// time threads spent runnable but not running (macOS), nanoseconds.
    pub runnable_ns: Option<u64>,
    /// physical footprint (macOS), resident set (Linux), private bytes (Windows).
    pub footprint: u64,
    pub footprint_lifetime_max: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub swap_used: Option<u64>,
}

/// Read this process's counters now.
pub fn proc_sample() -> ProcSample {
    platform_proc_sample()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
extern "C" {
    /// libmalloc: return free pages of `zone` (null = every zone) to the kernel;
    /// `goal` 0 means as much as possible. Not exposed by `libc`.
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: usize) -> usize;
}

/// Hand the allocator's free (not live) pages back to the OS and report this
/// process's footprint `(before, after)` in bytes. It only releases memory
/// that no allocation holds, so no result can change. `None` where the
/// platform has no such call (Windows, musl).
pub fn release_free_memory() -> Option<(u64, u64)> {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        all(target_os = "linux", target_env = "gnu")
    ))]
    {
        let before = proc_sample().footprint;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        // SAFETY: a null zone means "all zones"; the call only returns free pages.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: `malloc_trim` only releases free heap memory.
        unsafe {
            libc::malloc_trim(0);
        }
        let after = proc_sample().footprint;
        Some((before, after))
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        all(target_os = "linux", target_env = "gnu")
    )))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn sysctl_raw<T: Copy>(name: &str, out: &mut T) -> bool {
    let cname = match std::ffi::CString::new(name) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut len = std::mem::size_of::<T>();
    // SAFETY: `cname` is NUL-terminated, `out`/`len` describe one `T` owned by
    // the caller, and the new-value pointer is null (a pure read).
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            out as *mut T as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    rc == 0 && len == std::mem::size_of::<T>()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn sysctl_u32(name: &str) -> Option<u32> {
    let mut v: u32 = 0;
    if sysctl_raw(name, &mut v) {
        Some(v)
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_core_split() -> (Option<usize>, Option<usize>) {
    match sysctl_u32("hw.perflevel0.physicalcpu") {
        Some(p) => (
            Some(p as usize),
            Some(sysctl_u32("hw.perflevel1.physicalcpu").unwrap_or(0) as usize),
        ),
        None => (None, None),
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_cpu_quota(_logical: usize) -> Option<f64> {
    None
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_memory() -> (Option<u64>, Option<u64>) {
    let mut size: u64 = 0;
    if sysctl_raw("hw.memsize", &mut size) && size > 0 {
        (Some(size), None)
    } else {
        tracing::warn!("hw.memsize probe failed; physical memory unknown");
        (None, None)
    }
}

/// Free + inactive + purgeable pages (the kernel's `free_count` already
/// includes the speculative pages, so they are not added twice) x page size.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(deprecated)]
pub fn available_memory() -> Option<u64> {
    // SAFETY: `vm_statistics64` is plain integers; `count` is its length in
    // `integer_t` words, the documented calling convention of
    // `host_statistics64(HOST_VM_INFO64)`.
    unsafe {
        let mut info: libc::vm_statistics64 = std::mem::zeroed();
        let mut count = (std::mem::size_of::<libc::vm_statistics64>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let rc = libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            &mut info as *mut libc::vm_statistics64 as libc::host_info64_t,
            &mut count,
        );
        if rc != 0 {
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, || {
                tracing::warn!(rc, "host_statistics64 failed; available memory unknown")
            });
            return None;
        }
        let page = libc::sysconf(libc::_SC_PAGESIZE);
        if page <= 0 {
            return None;
        }
        let pages =
            info.free_count as u64 + info.inactive_count as u64 + info.purgeable_count as u64;
        Some(pages.saturating_mul(page as u64))
    }
}

/// `struct rusage_info_v6` from the macOS SDK's `sys/resource.h` (libc has no
/// binding). Field order is the ABI; `RUSAGE_INFO_V6_SIZE` pins the size.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct RusageInfoV6 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
    ri_cpu_time_qos_default: u64,
    ri_cpu_time_qos_maintenance: u64,
    ri_cpu_time_qos_background: u64,
    ri_cpu_time_qos_utility: u64,
    ri_cpu_time_qos_legacy: u64,
    ri_cpu_time_qos_user_initiated: u64,
    ri_cpu_time_qos_user_interactive: u64,
    ri_billed_system_time: u64,
    ri_serviced_system_time: u64,
    ri_logical_writes: u64,
    ri_lifetime_max_phys_footprint: u64,
    ri_instructions: u64,
    ri_cycles: u64,
    ri_billed_energy: u64,
    ri_serviced_energy: u64,
    ri_interval_max_phys_footprint: u64,
    ri_runnable_time: u64,
    ri_flags: u64,
    ri_user_ptime: u64,
    ri_system_ptime: u64,
    ri_pinstructions: u64,
    ri_pcycles: u64,
    ri_energy_nj: u64,
    ri_penergy_nj: u64,
    ri_secure_time_in_system: u64,
    ri_secure_ptime_in_system: u64,
    ri_neural_footprint: u64,
    ri_lifetime_max_neural_footprint: u64,
    ri_interval_max_neural_footprint: u64,
    ri_conclave_footprint: u64,
    ri_page_wait_time_mach: u64,
    ri_page_cache_hits: u64,
    ri_reserved: [u64; 6],
}

#[cfg(all(test, any(target_os = "macos", target_os = "ios")))]
const RUSAGE_INFO_V6_SIZE: usize = 464;

#[cfg(any(target_os = "macos", target_os = "ios"))]
const RUSAGE_INFO_V6: libc::c_int = 6;
/// macOS 11 / 12 stop at flavor 4 (V5 on 12); V6 extends V4 field for field.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const RUSAGE_INFO_V4: libc::c_int = 4;

/// Mach absolute-time ticks -> nanoseconds (the ratio is NOT 1:1 on every Mac).
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(deprecated)]
fn mach_ticks_to_ns(ticks: u64) -> u64 {
    static RATIO: OnceLock<(u32, u32)> = OnceLock::new();
    let (numer, denom) = *RATIO.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `tb` is a plain two-u32 struct owned by this frame.
        let rc = unsafe { libc::mach_timebase_info(&mut tb) };
        if rc == 0 && tb.denom != 0 {
            (tb.numer, tb.denom)
        } else {
            tracing::warn!(rc, "mach_timebase_info failed; assuming a 1:1 tick");
            (1, 1)
        }
    });
    ((ticks as u128 * numer as u128) / denom as u128) as u64
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_proc_sample() -> ProcSample {
    static V6_UNSUPPORTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static FAILED_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let mut ri: RusageInfoV6 = unsafe { std::mem::zeroed() };
    let call = |flavor: libc::c_int, ri: &mut RusageInfoV6| {
        // SAFETY: `ri` is a correctly sized, writable mirror of the SDK
        // struct; `proc_pid_rusage` fills at most that flavor's size of it
        // (V4 is a prefix of V6).
        unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                flavor,
                ri as *mut RusageInfoV6 as *mut libc::rusage_info_t as *mut _,
            )
        }
    };
    let mut flavor = if V6_UNSUPPORTED.load(std::sync::atomic::Ordering::Relaxed) {
        RUSAGE_INFO_V4
    } else {
        RUSAGE_INFO_V6
    };
    let mut rc = call(flavor, &mut ri);
    if rc != 0 && flavor == RUSAGE_INFO_V6 {
        // An older macOS (11 / 12) refuses flavor 6: remember it, fall back to V4.
        V6_UNSUPPORTED.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            rc,
            "proc_pid_rusage flavor 6 unsupported; using flavor 4 (no per-class cpu time)"
        );
        flavor = RUSAGE_INFO_V4;
        rc = call(flavor, &mut ri);
    }
    if rc != 0 {
        warn_once(&FAILED_WARNED, || {
            tracing::warn!(
                rc,
                flavor,
                "proc_pid_rusage failed; process sample unavailable"
            )
        });
        return ProcSample::default();
    }
    let hybrid = flavor == RUSAGE_INFO_V6 && profile().eff_cores.unwrap_or(0) > 0;
    let mut sample = ProcSample {
        cpu_ns: mach_ticks_to_ns(ri.ri_user_time.saturating_add(ri.ri_system_time)),
        p_cpu_ns: hybrid
            .then(|| mach_ticks_to_ns(ri.ri_user_ptime.saturating_add(ri.ri_system_ptime))),
        runnable_ns: Some(mach_ticks_to_ns(ri.ri_runnable_time)),
        footprint: ri.ri_phys_footprint,
        footprint_lifetime_max: ri.ri_lifetime_max_phys_footprint,
        disk_read: ri.ri_diskio_bytesread,
        disk_write: ri.ri_diskio_byteswritten,
        swap_used: None,
    };
    // SAFETY: `xsw_usage` is plain integers; sysctl fills exactly one.
    let mut swap: libc::xsw_usage = unsafe { std::mem::zeroed() };
    if sysctl_raw("vm.swapusage", &mut swap) {
        sample.swap_used = Some(swap.xsu_used);
    }
    sample
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "linux")]
fn platform_core_split() -> (Option<usize>, Option<usize>) {
    // Distinct physical cores (a hyper-threaded P core lists two cpus).
    let count = |p: &str| {
        let cpus = read_trimmed(p).and_then(|t| expand_cpu_list(&t))?;
        Some(count_distinct_cores(&cpus, |c| {
            let base = format!("/sys/devices/system/cpu/cpu{c}/topology");
            let core = read_trimmed(&format!("{base}/core_id"))?
                .trim()
                .parse()
                .ok()?;
            let pkg = read_trimmed(&format!("{base}/physical_package_id"))?
                .trim()
                .parse()
                .ok()?;
            Some((pkg, core))
        }))
    };
    match (
        count("/sys/devices/cpu_core/cpus"),
        count("/sys/devices/cpu_atom/cpus"),
    ) {
        (Some(p), Some(e)) => (Some(p), Some(e)),
        _ => (None, None),
    }
}

#[cfg(target_os = "linux")]
fn platform_cpu_quota(_logical: usize) -> Option<f64> {
    if let Some(t) = read_trimmed("/sys/fs/cgroup/cpu.max") {
        return parse_cpu_max(&t);
    }
    for dir in ["/sys/fs/cgroup/cpu", "/sys/fs/cgroup/cpu,cpuacct"] {
        let q = read_trimmed(&format!("{dir}/cpu.cfs_quota_us"));
        let p = read_trimmed(&format!("{dir}/cpu.cfs_period_us"));
        if let (Some(q), Some(p)) = (q, p) {
            return parse_cfs_pair(&q, &p);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn platform_memory() -> (Option<u64>, Option<u64>) {
    let total = match read_trimmed("/proc/meminfo") {
        Some(m) => parse_kb_field(&m, "MemTotal:"),
        None => {
            tracing::warn!("/proc/meminfo unreadable; physical memory unknown");
            None
        }
    };
    let v2 = read_trimmed("/sys/fs/cgroup/memory.max");
    let v1 = read_trimmed("/sys/fs/cgroup/memory/memory.limit_in_bytes");
    (total, cgroup_limit(v2.as_deref(), v1.as_deref()))
}

#[cfg(target_os = "linux")]
pub fn available_memory() -> Option<u64> {
    let avail = parse_kb_field(&read_trimmed("/proc/meminfo")?, "MemAvailable:")?;
    let limit = profile().mem_limit;
    let current = read_trimmed("/sys/fs/cgroup/memory.current")
        .or_else(|| read_trimmed("/sys/fs/cgroup/memory/memory.usage_in_bytes"))
        .and_then(|t| t.trim().parse::<u64>().ok());
    // Reclaimable page cache is not "used": subtract the inactive file pages.
    let inactive = read_trimmed("/sys/fs/cgroup/memory.stat")
        .and_then(|t| parse_stat_field(&t, "inactive_file"))
        .or_else(|| {
            read_trimmed("/sys/fs/cgroup/memory/memory.stat")
                .and_then(|t| parse_stat_field(&t, "total_inactive_file"))
        })
        .unwrap_or(0);
    Some(match (limit, current) {
        (Some(l), Some(c)) => avail.min(cgroup_available(l, c, inactive)),
        _ => avail,
    })
}

#[cfg(target_os = "linux")]
fn platform_proc_sample() -> ProcSample {
    let mut s = ProcSample::default();
    // SAFETY: `rusage` is plain integers; getrusage fills it for RUSAGE_SELF.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0 {
        let tv = |t: libc::timeval| (t.tv_sec as u64) * 1_000_000_000 + (t.tv_usec as u64) * 1_000;
        s.cpu_ns = tv(ru.ru_utime).saturating_add(tv(ru.ru_stime));
    } else {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        warn_once(&WARNED, || tracing::warn!("getrusage failed; cpu time unknown"));
    }
    if let Some(st) = read_trimmed("/proc/self/status") {
        s.footprint = parse_kb_field(&st, "VmRSS:").unwrap_or(0);
        s.footprint_lifetime_max = parse_kb_field(&st, "VmHWM:").unwrap_or(0);
        s.swap_used = parse_kb_field(&st, "VmSwap:");
    }
    if let Some((r, w)) = read_trimmed("/proc/self/io").and_then(|t| parse_proc_io(&t)) {
        s.disk_read = r;
        s.disk_write = w;
    }
    s
}

// ---------------------------------------------------------------------------
// Windows (cannot be compiled on the dev Mac; the GitHub run checks it)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn platform_core_split() -> (Option<usize>, Option<usize>) {
    use std::collections::{BTreeMap, BTreeSet};
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };
    let mut len: u32 = 0;
    // SAFETY: a null buffer with length 0 asks for the required size.
    unsafe {
        GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut len, std::ptr::null_mut(), 0)
    };
    if len == 0 {
        return (None, None);
    }
    // u64 words keep the buffer 8-byte aligned for the records inside it.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes.
    let ok = unsafe {
        GetSystemCpuSetInformation(
            buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION,
            len,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        tracing::warn!("GetSystemCpuSetInformation failed; efficiency classes unknown");
        return (None, None);
    }
    let base = buf.as_ptr() as *const u8;
    let mut cores: BTreeMap<u8, BTreeSet<(u16, u8)>> = BTreeMap::new();
    let mut off = 0usize;
    while off + std::mem::size_of::<u32>() * 2 <= len as usize {
        // SAFETY: `off` stays inside the `len` bytes the call wrote; each
        // record starts 8-byte aligned (records are padded by the OS).
        let rec = unsafe { &*(base.add(off) as *const SYSTEM_CPU_SET_INFORMATION) };
        if rec.Size == 0 {
            break;
        }
        if rec.Type == 0 {
            // CpuSetInformation
            let cs = unsafe { rec.Anonymous.CpuSet };
            cores
                .entry(cs.EfficiencyClass)
                .or_default()
                .insert((cs.Group, cs.CoreIndex));
        }
        off += rec.Size as usize;
    }
    let classes: Vec<(u8, usize)> = cores.into_iter().map(|(c, s)| (c, s.len())).collect();
    split_efficiency_classes(&classes)
}

#[cfg(windows)]
fn platform_cpu_quota(logical: usize) -> Option<f64> {
    use windows_sys::Win32::System::JobObjects::{
        JobObjectCpuRateControlInformation, QueryInformationJobObject,
        JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
        JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
    };
    // SAFETY: a plain POD out-struct; a null job handle queries the job the
    // calling process belongs to (the call fails when there is none).
    let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectCpuRateControlInformation,
            &mut info as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let flags = info.ControlFlags;
    if flags & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE == 0
        || flags & JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP == 0
    {
        return None;
    }
    // CpuRate is a percentage of the whole machine in 1/100 percent.
    let rate = unsafe { info.Anonymous.CpuRate };
    if rate == 0 {
        return None;
    }
    Some(rate as f64 / 10_000.0 * logical as f64)
}

#[cfg(windows)]
fn platform_memory() -> (Option<u64>, Option<u64>) {
    use windows_sys::Win32::System::JobObjects::{
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is a plain POD struct; zeroing it and stamping
    // dwLength is exactly the documented calling convention.
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let total = if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 && st.ullTotalPhys > 0 {
        Some(st.ullTotalPhys)
    } else {
        tracing::warn!("GlobalMemoryStatusEx failed; physical memory unknown");
        None
    };
    // SAFETY: as in `platform_cpu_quota`.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    let mut limit: Option<u64> = None;
    if ok != 0 {
        let flags = info.BasicLimitInformation.LimitFlags;
        if flags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0 && info.JobMemoryLimit > 0 {
            limit = Some(info.JobMemoryLimit as u64);
        }
        if flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY != 0 && info.ProcessMemoryLimit > 0 {
            let p = info.ProcessMemoryLimit as u64;
            limit = Some(limit.map_or(p, |l| l.min(p)));
        }
    }
    (total, limit)
}

#[cfg(windows)]
pub fn available_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: see `platform_memory`.
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 {
        Some(st.ullAvailPhys)
    } else {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        warn_once(&WARNED, || {
            tracing::warn!("GlobalMemoryStatusEx failed; available memory unknown")
        });
        None
    }
}

#[cfg(windows)]
fn platform_proc_sample() -> ProcSample {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessIoCounters, GetProcessTimes, IO_COUNTERS,
    };
    let mut s = ProcSample::default();
    let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) * 100;
    // SAFETY: all out-structs are plain PODs owned by this frame; the
    // pseudo-handle from GetCurrentProcess needs no close.
    unsafe {
        let h = GetCurrentProcess();
        let z = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut c, mut e, mut k, mut u) = (z, z, z, z);
        if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) != 0 {
            s.cpu_ns = ft(k).saturating_add(ft(u));
        } else {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, || tracing::warn!("GetProcessTimes failed; cpu time unknown"));
        }
        let mut mem: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        mem.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(
            h,
            &mut mem as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            mem.cb,
        ) != 0
        {
            s.footprint = mem.PrivateUsage as u64;
            s.footprint_lifetime_max = mem.PeakPagefileUsage as u64;
        } else {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, || tracing::warn!("GetProcessMemoryInfo failed; footprint unknown"));
        }
        let mut io: IO_COUNTERS = std::mem::zeroed();
        if GetProcessIoCounters(h, &mut io) != 0 {
            s.disk_read = io.ReadTransferCount;
            s.disk_write = io.WriteTransferCount;
        } else {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, || {
                tracing::warn!("GetProcessIoCounters failed; disk counters unknown")
            });
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Anything else
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_core_split() -> (Option<usize>, Option<usize>) {
    (None, None)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_cpu_quota(_logical: usize) -> Option<f64> {
    None
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
pub fn available_memory() -> Option<u64> {
    None
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_proc_sample() -> ProcSample {
    ProcSample::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prof(
        logical: usize,
        quota: Option<f64>,
        total: Option<u64>,
        limit: Option<u64>,
    ) -> MachineProfile {
        MachineProfile {
            logical_cpus: logical,
            perf_cores: None,
            eff_cores: None,
            cpu_quota: quota,
            mem_total: total,
            mem_limit: limit,
        }
    }

    #[test]
    fn distinct_cores_collapse_hyperthread_siblings() {
        assert_eq!(expand_cpu_list("0-3,8"), Some(vec![0, 1, 2, 3, 8]));
        assert_eq!(expand_cpu_list(""), Some(vec![]));
        assert_eq!(expand_cpu_list("3-1"), None);
        // cpus 0/1 and 2/3 are sibling pairs on package 0; cpu 8 is alone.
        let ids = |c: usize| Some((0i64, if c == 8 { 8 } else { (c / 2) as i64 }));
        assert_eq!(count_distinct_cores(&[0, 1, 2, 3, 8], ids), 3);
        // unreadable topology counts each such cpu as its own core
        assert_eq!(count_distinct_cores(&[0, 1], |_| None), 2);
        // same core id on two packages is two cores
        assert_eq!(count_distinct_cores(&[0, 1], |c| Some((c as i64, 0))), 2);
    }

    #[test]
    fn cgroup_available_subtracts_reclaimable_cache() {
        let v2 = "anon 100\ninactive_file 4096\nactive_file 8\n";
        assert_eq!(parse_stat_field(v2, "inactive_file"), Some(4096));
        assert_eq!(parse_stat_field(v2, "total_inactive_file"), None);
        let v1 = "cache 1\ntotal_inactive_file 77\n";
        assert_eq!(parse_stat_field(v1, "total_inactive_file"), Some(77));
        assert_eq!(cgroup_available(10_000, 9_000, 4_000), 5_000);
        assert_eq!(cgroup_available(10_000, 9_000, 20_000), 10_000);
        assert_eq!(cgroup_available(1_000, 9_000, 0), 0);
    }

    #[test]
    fn cpu_max_parses_quota_and_unlimited() {
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_cpu_max("200000 100000\n"), Some(2.0));
        assert_eq!(parse_cpu_max("150000 100000"), Some(1.5));
        assert_eq!(parse_cpu_max("garbage"), None);
        assert_eq!(parse_cpu_max(""), None);
        assert_eq!(parse_cpu_max("100000 0"), None);
    }

    #[test]
    fn cfs_pair_parses_and_minus_one_is_none() {
        assert_eq!(parse_cfs_pair("-1\n", "100000\n"), None);
        assert_eq!(parse_cfs_pair("400000\n", "100000\n"), Some(4.0));
        assert_eq!(parse_cfs_pair("x", "100000"), None);
        assert_eq!(parse_cfs_pair("100000", ""), None);
    }

    #[test]
    fn cpu_list_counts_ranges_and_singles() {
        assert_eq!(parse_cpu_list("0-7,16\n"), Some(9));
        assert_eq!(parse_cpu_list("0-15"), Some(16));
        assert_eq!(parse_cpu_list("3"), Some(1));
        assert_eq!(parse_cpu_list(""), Some(0));
        assert_eq!(parse_cpu_list("7-3"), None);
        assert_eq!(parse_cpu_list("a-b"), None);
    }

    #[test]
    fn kb_fields_and_proc_io_parse() {
        let status = "Name:\tx\nVmRSS:\t  2048 kB\nVmSwap:\t 0 kB\n";
        assert_eq!(parse_kb_field(status, "VmRSS:"), Some(2048 * 1024));
        assert_eq!(parse_kb_field(status, "VmSwap:"), Some(0));
        assert_eq!(parse_kb_field(status, "VmHWM:"), None);
        assert_eq!(
            parse_kb_field("MemAvailable: junk kB", "MemAvailable:"),
            None
        );
        let io = "rchar: 1\nread_bytes: 4096\nwrite_bytes: 8192\n";
        assert_eq!(parse_proc_io(io), Some((4096, 8192)));
        assert_eq!(parse_proc_io("read_bytes: 1\n"), None);
    }

    #[test]
    fn efficiency_classes_split() {
        assert_eq!(split_efficiency_classes(&[]), (None, None));
        assert_eq!(split_efficiency_classes(&[(0, 8)]), (None, None));
        assert_eq!(
            split_efficiency_classes(&[(0, 4), (1, 8)]),
            (Some(8), Some(4))
        );
        assert_eq!(
            split_efficiency_classes(&[(0, 2), (1, 4), (2, 6)]),
            (Some(6), Some(6))
        );
    }

    #[test]
    fn pool_threads_env_invalid_quota_and_auto() {
        let p = prof(24, None, None, None);
        assert_eq!(pool_threads_with(&p, Some("6")), (6, "env"));
        assert_eq!(pool_threads_with(&p, Some(" 40 ")), (40, "env"));
        assert_eq!(pool_threads_with(&p, Some("0")), (24, "auto"));
        assert_eq!(pool_threads_with(&p, Some("many")), (24, "auto"));
        assert_eq!(pool_threads_with(&p, None), (24, "auto"), "no 16 ceiling");
        let q = prof(24, Some(2.5), None, None);
        let (n, why) = pool_threads_with(&q, None);
        assert_eq!(n, 3);
        assert_eq!(why, if cfg!(windows) { "job" } else { "cgroup" });
        assert_eq!(
            pool_threads_with(&prof(2, Some(8.0), None, None), None),
            (2, "auto"),
            "a quota that does not bind is not the reason"
        );
        assert_eq!(
            pool_threads_with(&prof(4, Some(4.0), None, None), None),
            (4, "auto")
        );
    }

    #[test]
    fn memory_budget_setting_auto_unknown() {
        let gib = 1u64 << 30;
        let p = prof(8, None, Some(16 * gib), None);
        assert_eq!(memory_budget_from(0, &p), Some((4 * gib, "auto")));
        assert_eq!(memory_budget_from(2048, &p), Some((2 * gib, "setting")));
        let c = prof(8, None, Some(16 * gib), Some(2 * gib));
        assert_eq!(
            memory_budget_from(0, &c),
            Some((gib / 2, "auto")),
            "the limit, not the host"
        );
        assert_eq!(memory_budget_from(0, &prof(8, None, None, None)), None);
        assert_eq!(
            memory_budget_from(512, &prof(8, None, None, None)),
            Some((512 << 20, "setting"))
        );
    }

    #[test]
    fn memory_budget_setting_is_clamped_to_the_effective_memory() {
        let gib = 1u64 << 30;
        let p = prof(8, None, Some(16 * gib), None);
        // above the machine: clamped
        assert_eq!(
            memory_budget_from(64 * 1024, &p),
            Some((16 * gib, "setting-clamped"))
        );
        // above the container limit: clamped to the limit, not the host
        let c = prof(8, None, Some(16 * gib), Some(2 * gib));
        assert_eq!(
            memory_budget_from(4096, &c),
            Some((2 * gib, "setting-clamped"))
        );
        // exactly the memory: not clamped
        assert_eq!(
            memory_budget_from(16 * 1024, &p),
            Some((16 * gib, "setting"))
        );
        // 1 MiB stays valid (the admission-1 arm)
        assert_eq!(memory_budget_from(1, &p), Some((1 << 20, "setting")));
    }

    #[test]
    fn profile_is_sane_and_total_ram_matches() {
        let p = profile();
        assert!(p.logical_cpus >= 1);
        assert_eq!(total_ram_bytes(), p.effective_mem());
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(
            total_ram_bytes().is_some(),
            "the RAM probe must work on a supported platform"
        );
    }

    /// Logs the profile the way `compute budget resolved` does (run with
    /// `--nocapture`); asserts only what holds on every machine.
    #[test]
    fn profile_line_for_the_report() {
        let p = profile();
        let (threads, why) = pool_threads(p);
        eprintln!(
            "compute budget resolved: logical_cpus={} perf_cores={:?} eff_cores={:?} cpu_quota={:?} mem_total_bytes={:?} \
             mem_limit_bytes={:?} mem_available_bytes={:?} memory_budget={:?} pool_threads={threads} pool_why={why}",
            p.logical_cpus,
            p.perf_cores,
            p.eff_cores,
            p.cpu_quota,
            p.mem_total,
            p.mem_limit,
            available_memory(),
            memory_budget_from(0, p),
        );
        assert!(threads >= 1);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn release_free_memory_reports_a_footprint_that_does_not_grow() {
        let (before, after) = release_free_memory().expect("macOS has a release call");
        assert!(before > 0);
        assert!(
            after <= before + (1 << 20),
            "footprint grew across the release: {before} -> {after}"
        );
    }

    #[test]
    fn proc_sample_reads_something() {
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        let s = proc_sample();
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            assert!(s.cpu_ns > 0, "cpu time must be readable: {s:?}");
            assert!(s.footprint > 0, "footprint must be readable: {s:?}");
            assert!(s.footprint_lifetime_max >= s.footprint.min(s.footprint_lifetime_max));
        }
        let _ = s;
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn rusage_v6_mirror_matches_the_sdk_size() {
        assert_eq!(std::mem::size_of::<RusageInfoV6>(), RUSAGE_INFO_V6_SIZE);
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn macos_available_memory_is_below_total() {
        let a = available_memory().expect("host_statistics64 must work");
        assert!(a > 0 && a <= total_ram_bytes().unwrap(), "available {a}");
    }
}
