//! The platform memory probe: `platform_memory()` with its four platform variants and its helpers, and
//! `total_ram_bytes`. Moved from Athenaeum core's `services::machine`; core keeps the rest of its machine profile.
//!
//! Every parser is a pure function on a `&str` so it is unit-tested on every platform; only the file / syscall
//! read is `cfg`-gated. An unknown value is `None`, never a guess.

use std::sync::OnceLock;

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

/// Parse one cgroup memory-limit file. `None` means "no limit here": cgroup v2
/// writes the literal `max`, and v1 writes a sentinel near `u64::MAX` — a
/// number that large is not a container limit, it is the absence of one.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
#[doc(hidden)]
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
/// wins when it names a real limit; `max` (or absent) falls through to v1.
#[cfg_attr(not(test), allow(dead_code))]
#[doc(hidden)]
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

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
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

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
pub fn platform_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// The probed pair behind [`total_ram_bytes`], resolved once per process.
static MEMORY: OnceLock<(Option<u64>, Option<u64>)> = OnceLock::new();

/// Physical RAM this process may actually use, in bytes: the container / job limit when one is set below physical
/// memory, else physical memory. `None` when the platform cannot say. The same rule as Athenaeum core's
/// `MachineProfile::effective_mem`, over this crate's own probe.
///
/// On Linux this is `min(MemTotal, container limit)`: `/proc/meminfo` reports the HOST's RAM inside a container,
/// so without the cgroup read a 2 GB container would size an 8 GiB budget and be OOM-killed.
pub fn total_ram_bytes() -> Option<u64> {
    let (total, limit) = *MEMORY.get_or_init(platform_memory);
    effective_memory(total, limit)
}

/// The effective-memory rule over a probed pair: a limit counts only when it is below physical memory (or physical
/// memory is unknown); the result is then the limit when set, else physical memory.
fn effective_memory(total: Option<u64>, limit: Option<u64>) -> Option<u64> {
    let limit = match (total, limit) {
        (Some(t), Some(l)) if l < t => Some(l),
        (None, Some(l)) => Some(l),
        _ => None,
    };
    match (total, limit) {
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
