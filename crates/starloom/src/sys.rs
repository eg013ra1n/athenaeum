//! The platform memory probe: `platform_memory()` with its four platform variants and its helpers, and
//! `total_ram_bytes`, the one answer to "how much RAM may this process use" that the stacking budgets read.
//!
//! Every parser is a pure function on a `&str` so it is unit-tested on every platform; only the file / syscall
//! read is `cfg`-gated. An unknown value is `None`, never a guess.

use std::sync::OnceLock;

/// One `Key:   123 kB` line of `/proc/meminfo` or `/proc/self/status`, in bytes. `key` includes the colon
/// (`"MemTotal:"`); `None` when the key is absent or its value is not a number.
pub fn parse_kb_field(text: &str, key: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Parse one cgroup memory-limit file. `None` means "no limit here": cgroup v2
/// writes the literal `max`, and v1 writes a sentinel near `u64::MAX` — a
/// number that large is not a container limit, it is the absence of one.
pub fn parse_cgroup_limit(text: &str) -> Option<u64> {
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
/// wins when it names a real limit; `max` (or absent) falls through to v1. `None` when `MemTotal:` is absent.
pub fn linux_total_ram_from(
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

fn cgroup_limit(v2: Option<&str>, v1: Option<&str>) -> Option<u64> {
    v2.and_then(parse_cgroup_limit)
        .or_else(|| v1.and_then(parse_cgroup_limit))
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

/// `(physical memory, container / job memory limit)` in bytes; `None` where the platform cannot say. One variant
/// per platform family below; the limit is whatever the platform reports, not yet checked against the physical total.
///
/// macOS and iOS: physical memory from `sysctl hw.memsize`; there is no container limit, so the second value is
/// always `None`. Both are `None` (and a warning is logged) when the sysctl fails.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn platform_memory() -> (Option<u64>, Option<u64>) {
    let mut size: u64 = 0;
    if sysctl_raw("hw.memsize", &mut size) && size > 0 {
        (Some(size), None)
    } else {
        tracing::warn!("hw.memsize probe failed; physical memory unknown");
        (None, None)
    }
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `(physical memory, container / job memory limit)` in bytes; `None` where the platform cannot say. One variant
/// per platform family.
///
/// Linux: physical memory is `MemTotal` from `/proc/meminfo` (`None`, with a warning, when the file cannot be read;
/// `None` without one when it has no `MemTotal`). The limit is the cgroup v2 `memory.max`, else the cgroup v1 `memory.limit_in_bytes`; `None`
/// when neither file exists or names a real limit.
#[cfg(target_os = "linux")]
pub fn platform_memory() -> (Option<u64>, Option<u64>) {
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

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// `(physical memory, container / job memory limit)` in bytes; `None` where the platform cannot say. One variant
/// per platform family.
///
/// Windows: physical memory from `GlobalMemoryStatusEx` (`None`, with a warning, when the call fails). The limit is
/// the calling process's job-object memory limit: the smaller of the job limit and the per-process limit when both
/// are set, `None` when the process is in no job or the job sets neither.
#[cfg(windows)]
pub fn platform_memory() -> (Option<u64>, Option<u64>) {
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
    // SAFETY: the struct is plain integers; a null job handle queries the job of the calling process.
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

// ---------------------------------------------------------------------------
// Anything else
// ---------------------------------------------------------------------------

/// `(physical memory, container / job memory limit)` in bytes; `None` where the platform cannot say. One variant
/// per platform family.
///
/// Any other platform: nothing is probed, so both values are always `None` and [`total_ram_bytes`] is `None`.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
pub fn platform_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// The probed pair behind [`total_ram_bytes`], resolved once per process.
static MEMORY: OnceLock<(Option<u64>, Option<u64>)> = OnceLock::new();

/// Physical RAM this process may actually use, in bytes: the container / job limit when one is set below physical
/// memory, else physical memory. Probed once per process.
///
/// `None` when the platform cannot say: the physical total and the limit are both unknown (an unreadable
/// `/proc/meminfo`, a failed `sysctl` or `GlobalMemoryStatusEx`, a platform with no probe). The caller must pick its
/// own fallback; none is applied here. Each consumer documents the one it takes.
///
/// On Linux this is `min(MemTotal, container limit)`: `/proc/meminfo` reports the HOST's RAM inside a container,
/// so without the cgroup read a 2 GB container would size an 8 GiB budget and be OOM-killed.
pub fn total_ram_bytes() -> Option<u64> {
    let (total, limit) = *MEMORY.get_or_init(platform_memory);
    effective_memory(total, limit)
}

/// The container or job memory limit of a probed pair, kept only when it binds: a limit counts when it is below
/// physical memory, or when physical memory is unknown. A limit equal to or above physical memory is no limit
/// (`None`).
pub fn binding_limit(total: Option<u64>, limit: Option<u64>) -> Option<u64> {
    match (total, limit) {
        (Some(t), Some(l)) if l < t => Some(l),
        (None, Some(l)) => Some(l),
        _ => None,
    }
}

/// The effective-memory rule over a probed pair: the minimum of the physical total and the [`binding_limit`]; the
/// limit alone when the total is unknown; `None` when neither is known.
pub fn effective_memory(total: Option<u64>, limit: Option<u64>) -> Option<u64> {
    match (total, binding_limit(total, limit)) {
        (Some(t), Some(l)) => Some(t.min(l)),
        (Some(t), None) => Some(t),
        (None, l) => l,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_memory_is_the_limit_only_when_it_binds() {
        assert_eq!(effective_memory(Some(16), Some(4)), Some(4));
        assert_eq!(effective_memory(Some(16), Some(16)), Some(16));
        assert_eq!(effective_memory(Some(16), Some(64)), Some(16), "a limit above physical memory is no limit");
        assert_eq!(effective_memory(Some(16), None), Some(16));
        assert_eq!(effective_memory(None, Some(4)), Some(4));
        assert_eq!(effective_memory(None, None), None);
    }

    #[test]
    fn binding_limit_keeps_only_a_limit_below_physical_memory() {
        assert_eq!(binding_limit(Some(16), Some(4)), Some(4));
        assert_eq!(binding_limit(Some(16), Some(16)), None);
        assert_eq!(binding_limit(Some(16), Some(64)), None);
        assert_eq!(binding_limit(Some(16), None), None);
        assert_eq!(binding_limit(None, Some(4)), Some(4));
        assert_eq!(binding_limit(None, None), None);
    }

    #[test]
    fn kb_fields_and_cgroup_limits_parse() {
        let status = "VmRSS:      2048 kB\nVmSwap:        0 kB\n";
        assert_eq!(parse_kb_field(status, "VmRSS:"), Some(2048 * 1024));
        assert_eq!(parse_kb_field(status, "VmHWM:"), None);
        assert_eq!(parse_cgroup_limit("2147483648\n"), Some(2_147_483_648));
        assert_eq!(parse_cgroup_limit("max\n"), None);
        assert_eq!(parse_cgroup_limit("9223372036854771712"), None);
        let meminfo = "MemTotal:        4194304 kB\nMemFree: 1 kB\n";
        assert_eq!(linux_total_ram_from(meminfo, Some("2147483648\n"), None), Some(2_147_483_648));
        assert_eq!(linux_total_ram_from(meminfo, Some("max\n"), None), Some(4_294_967_296));
    }

    #[test]
    fn total_ram_matches_the_probe_on_a_known_platform() {
        let ram = total_ram_bytes();
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(ram.is_some_and(|b| b > 0), "this platform reports its memory");
        let _ = ram;
        assert_eq!(total_ram_bytes(), ram, "resolved once per process");
    }
}
