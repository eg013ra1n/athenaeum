//! Resolves the working-memory budget for banded integration.
//!
//! Until 2026-09-06 this was a compile-time `256 * 1024 * 1024`. Profiling
//! (`docs/superpowers/research/2026-09-06-master-integration-io-profiling.md`)
//! measured that constant costing 5.3x on a 100-frame set: it yields 105-row
//! bands, so the reader crosses all 100 files forty times and gets 22 MB/s off
//! a drive that sustains 243 MB/s. The budget is a property of the machine,
//! so it is resolved from the machine.

use anyhow::Result;
use rusqlite::Connection;

use crate::settings::SettingsManager;

/// The pre-2026-09-06 constant. The policy is floored here so it can never
/// make any machine slower than it already is.
pub const MIN_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// 100 frames of 26 Mpx as `u16` is 5.2 GB; 8 GiB is where a large machine
/// reaches a single band and therefore whole-file sequential reads. Above
/// that the budget buys nothing an integration can spend.
pub const MAX_BUDGET_BYTES: usize = 8 * 1024 * 1024 * 1024;

/// Used when the RAM probe fails. Measured at 101.2 s against the old
/// constant's 241.6 s on the profiled set, and safe on any machine with the
/// 8 GB a 26 Mpx pipeline already needs.
pub const FALLBACK_BUDGET_BYTES: usize = 1024 * 1024 * 1024;

/// Bounds for an explicitly configured `integration.band_budget_mb`. The
/// floor is the same number as `MIN_BUDGET_BYTES` (256 MB = 256 MiB here) on
/// purpose: `per_job_budget`'s own `.max(MIN_BUDGET_BYTES)` floors every
/// resolved budget unconditionally, so a lower configured floor would be
/// silently overridden with no explanation — an honest configured window
/// must not advertise a minimum the backend refuses to honour.
///
/// KEEP IN SYNC with the `budgetNote` clamp window duplicated in
/// `src/pages/Settings.tsx` (the Calibration tab's "Integration memory
/// budget" control) — these constants are private and ts-rs has no way to
/// hand a plain `usize` across the boundary, so the frontend re-derives its
/// own copy to explain a clamp without a round trip. A change here with no
/// matching change there means the UI confidently states the wrong range.
const CONFIGURED_MIN_MB: usize = 256;
const CONFIGURED_MAX_MB: usize = 16384;

/// Physical RAM this process may actually use, in bytes.
///
/// On Linux this is `min(MemTotal, container limit)` — **load-bearing for the
/// Docker/web build**: `/proc/meminfo` reports the HOST's RAM inside a
/// container, so without the cgroup read a 2 GB container would size an 8 GiB
/// budget and be OOM-killed.
pub fn total_ram_bytes() -> Option<u64> {
    platform_total_ram()
}

#[cfg(target_os = "linux")]
fn platform_total_ram() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let v2 = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok();
    let v1 = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes").ok();
    linux_total_ram_from(&meminfo, v2.as_deref(), v1.as_deref())
}

/// Pure computation over `/proc/meminfo`'s content plus the two possible
/// cgroup limit-file contents. Split out of `platform_total_ram`'s Linux arm
/// (which does nothing else but read those three files) so the container-
/// clamping logic itself — not just `parse_cgroup_limit`'s number parsing —
/// has a test that can run on every dev machine, same treatment as
/// `parse_cgroup_limit` below.
///
/// `v2`/`v1` are `Some(contents)` when that file could be read, `None` when
/// it doesn't exist (no cgroup there, or a v1-only / non-container host for
/// `v2`). cgroup v2 wins when it names a real limit; `v2` holding the literal
/// `max` (or being absent) falls through to v1; neither present is `total`
/// unmodified.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_total_ram_from(meminfo: &str, v2: Option<&str>, v1: Option<&str>) -> Option<u64> {
    let mut total = None;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            total = Some(kb.saturating_mul(1024));
            break;
        }
    }
    let total = total?;
    let limit = v2.and_then(parse_cgroup_limit).or_else(|| v1.and_then(parse_cgroup_limit));
    Some(match limit {
        Some(l) => total.min(l),
        None => total,
    })
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_total_ram() -> Option<u64> {
    let mut size: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    // SAFETY: `name` is NUL-terminated, `size`/`len` are correctly sized and
    // owned by this frame, and the new-value pointer is null (a pure read).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr() as *const libc::c_char,
            &mut size as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && size > 0 { Some(size) } else { None }
}

#[cfg(windows)]
fn platform_total_ram() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is a plain POD struct; zeroing it and stamping
    // dwLength is exactly the documented calling convention.
    let mut st: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut st) } != 0 && st.ullTotalPhys > 0 {
        Some(st.ullTotalPhys)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn platform_total_ram() -> Option<u64> {
    None
}

/// Parse one cgroup memory-limit file. `None` means "no limit here": cgroup v2
/// writes the literal `max`, and v1 writes a sentinel near `u64::MAX` — a
/// number that large is not a container limit, it is the absence of one.
///
/// Only `platform_total_ram`'s Linux arm calls this in production; it stays a
/// plain (non-`#[cfg]`) function so its own unit tests run on every dev
/// machine, including this one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_cgroup_limit(text: &str) -> Option<u64> {
    const UNLIMITED_FLOOR: u64 = 1 << 62;
    let v: u64 = text.trim().parse().ok()?;
    if v == 0 || v >= UNLIMITED_FLOOR { None } else { Some(v) }
}

/// A quarter of physical RAM, clamped. A quarter and not a half because the
/// same process also holds the catalog, the render pipeline and the transfer
/// store — and because the OS needs page cache for the very files being read.
pub fn auto_budget_bytes() -> usize {
    match total_ram_bytes() {
        Some(ram) => (ram / 4).clamp(MIN_BUDGET_BYTES as u64, MAX_BUDGET_BYTES as u64) as usize,
        None => {
            tracing::warn!(
                fallback_mb = FALLBACK_BUDGET_BYTES / (1024 * 1024),
                "physical RAM probe failed — using the fallback band budget"
            );
            FALLBACK_BUDGET_BYTES
        }
    }
}

/// `0` is the auto sentinel; anything else is clamped to a sane window.
pub(crate) fn clamp_configured_mb(mb: usize) -> Option<usize> {
    if mb == 0 { None } else { Some(mb.clamp(CONFIGURED_MIN_MB, CONFIGURED_MAX_MB)) }
}

/// Split the machine-wide budget across the builds the compute queue may admit
/// at once, never below the old constant.
pub(crate) fn per_job_budget(total: usize, max_concurrent: usize) -> usize {
    (total / max_concurrent.max(1)).max(MIN_BUDGET_BYTES)
}

/// The budget one integration job may use, right now, on this machine.
pub fn resolve_budget_bytes(conn: &Connection, settings: &SettingsManager) -> Result<usize> {
    let configured = settings.get_integration_band_budget_mb(conn)?;
    let total = match clamp_configured_mb(configured) {
        Some(mb) => mb * 1024 * 1024,
        None => auto_budget_bytes(),
    };
    Ok(per_job_budget(total, settings.get_compute_max_concurrent(conn)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::keys;
    use rusqlite::Connection;

    /// The build must read the operator's setting, not a constant. Pinned at
    /// the resolver because the build itself needs a real 5 GB frame set to
    /// exercise end to end.
    #[test]
    fn resolver_honours_an_explicit_setting_over_auto() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let settings = SettingsManager::new();

        let auto = resolve_budget_bytes(&conn, &settings).unwrap();
        assert_eq!(auto, per_job_budget(auto_budget_bytes(), 1), "default 0 means auto");

        assert_ne!(
            auto,
            512 * 1024 * 1024,
            "fixture value must differ from this host's auto budget or the assertions below stop discriminating"
        );

        settings
            .persist_setting(&conn, keys::INTEGRATION_BAND_BUDGET_MB, "512")
            .unwrap();
        assert_eq!(resolve_budget_bytes(&conn, &settings).unwrap(), 512 * 1024 * 1024);

        settings
            .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, "2")
            .unwrap();
        assert_eq!(
            resolve_budget_bytes(&conn, &settings).unwrap(),
            MIN_BUDGET_BYTES,
            "512 MB split across 2 admitted jobs is 256 MB — the floor, not below it"
        );
    }

    #[test]
    fn auto_budget_is_a_quarter_of_ram_within_bounds() {
        let b = auto_budget_bytes();
        assert!(b >= MIN_BUDGET_BYTES, "auto {b} below the floor — would be slower than the old constant");
        assert!(b <= MAX_BUDGET_BYTES, "auto {b} above the cap");
        // Without this, a probe failure (wrong sysctl name, a failing
        // GlobalMemoryStatusEx, an unreadable /proc/meminfo) falls through to
        // the `if let` below and the test would pass vacuously — asserting a
        // tautology over three constants instead of exercising the probe.
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(total_ram_bytes().is_some(), "the RAM probe must work on a supported platform");
        if let Some(ram) = total_ram_bytes() {
            let want = (ram / 4).clamp(MIN_BUDGET_BYTES as u64, MAX_BUDGET_BYTES as u64) as usize;
            assert_eq!(b, want, "auto must be a quarter of {ram} bytes, clamped");
        }
    }

    #[test]
    fn linux_ram_uses_the_smaller_of_meminfo_and_the_container_limit() {
        let meminfo = "MemTotal:       16316196 kB\nMemFree:         1234567 kB\n";
        // Container limit (2 GiB) is well below MemTotal (~15.6 GiB) — min wins.
        let v2 = Some("2147483648\n");
        assert_eq!(linux_total_ram_from(meminfo, v2, None), Some(2_147_483_648));
    }

    #[test]
    fn linux_ram_prefers_cgroup_v2_over_v1_when_both_present() {
        let meminfo = "MemTotal:       16316196 kB\nMemFree:         1234567 kB\n";
        let v2 = Some("2147483648\n"); // 2 GiB
        let v1 = Some("1073741824\n"); // 1 GiB — must be ignored while v2 names a real limit
        assert_eq!(linux_total_ram_from(meminfo, v2, v1), Some(2_147_483_648));
    }

    #[test]
    fn linux_ram_falls_through_to_v1_when_v2_is_unlimited() {
        let meminfo = "MemTotal:       16316196 kB\nMemFree:         1234567 kB\n";
        let v2 = Some("max\n"); // cgroup v2 "no limit"
        let v1 = Some("1073741824\n"); // 1 GiB
        assert_eq!(linux_total_ram_from(meminfo, v2, v1), Some(1_073_741_824));
    }

    #[test]
    fn linux_ram_is_meminfo_total_with_no_cgroup_limit_at_all() {
        let meminfo = "MemTotal:       16316196 kB\nMemFree:         1234567 kB\n";
        assert_eq!(linux_total_ram_from(meminfo, None, None), Some(16_316_196 * 1024));
    }

    #[test]
    fn cgroup_v2_limit_parses_and_max_means_unlimited() {
        assert_eq!(parse_cgroup_limit("2147483648\n"), Some(2_147_483_648));
        assert_eq!(parse_cgroup_limit("max\n"), None, "'max' means no limit, not a limit of zero");
        assert_eq!(parse_cgroup_limit(""), None);
        assert_eq!(parse_cgroup_limit("not a number"), None);
        // cgroup v1 writes a sentinel near u64::MAX for "unlimited"; anything
        // that large is not a real container limit.
        assert_eq!(parse_cgroup_limit("9223372036854771712"), None);
    }

    #[test]
    fn configured_value_is_clamped_and_zero_means_auto() {
        assert_eq!(clamp_configured_mb(0), None, "0 is the auto sentinel, not a size");
        assert_eq!(clamp_configured_mb(1), Some(256), "clamps UP to the 256 MB floor");
        assert_eq!(clamp_configured_mb(512), Some(512));
        assert_eq!(clamp_configured_mb(999_999), Some(16384), "clamps DOWN to the 16 GB cap");
    }

    #[test]
    fn concurrency_divides_the_budget_but_never_below_the_floor() {
        assert_eq!(per_job_budget(4 * 1024 * 1024 * 1024, 1), 4 * 1024 * 1024 * 1024);
        assert_eq!(per_job_budget(4 * 1024 * 1024 * 1024, 4), 1024 * 1024 * 1024);
        assert_eq!(
            per_job_budget(512 * 1024 * 1024, 8),
            MIN_BUDGET_BYTES,
            "two admitted builds must not each claim a quarter of RAM, but neither may drop below the old constant"
        );
        assert_eq!(
            per_job_budget(4 * 1024 * 1024 * 1024, 0),
            4 * 1024 * 1024 * 1024,
            "a reported concurrency of 0 must not divide by zero — the .max(1) guard"
        );
    }
}
