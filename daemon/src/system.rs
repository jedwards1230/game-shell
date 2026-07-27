//! System and storage status IPC for the System/About settings page (#164).
//!
//! Both commands are purely stateless reads from procfs/sysfs and `/etc`:
//!
//! - `sys-status`     → `{"os":"…","kernel":"…","hostname":"…","uptime":"…"}`
//! - `storage-status` → `[{"mount":"…","size":…,"used":…,"avail":…,"pct":…}, …]`
//!
//! These are cross-platform (no Linux-only imports — they degrade gracefully on
//! non-Linux) and are served directly from `dispatch_stateless` without a round-
//! trip to the input runtime.

use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// sys-status
// ---------------------------------------------------------------------------

/// Read the `NAME=` field from `/etc/os-release`. Returns `"Unknown"` on
/// any error (file absent, no match, encoding issues).
fn os_name() -> String {
    let content = fs::read_to_string("/etc/os-release").unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("NAME=") {
            return rest.trim_matches('"').to_string();
        }
    }
    "Unknown".to_string()
}

/// Read the kernel release string from `/proc/sys/kernel/osrelease`
/// (equivalent to `uname -r`).
fn kernel_version() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|_| "Unknown".into())
        .trim()
        .to_string()
}

/// Read the hostname from `/proc/sys/kernel/hostname`.
fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "Unknown".into())
        .trim()
        .to_string()
}

/// Format an uptime in seconds as `Xd Xh Xm Xs`, eliding leading zero units
/// (e.g. `90` -> `"1m 30s"`, `2d 3h 5m 10s` for a multi-day uptime).
fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Parse the first whitespace-separated field of a `/proc/uptime` body into
/// whole seconds. Pure (no I/O) so the parse is unit-testable on any host.
///
/// `None` for an empty/malformed body, and for a negative value — a clock that
/// reports a negative uptime is nonsense, and `as u64` would wrap it into a
/// gigantic positive number.
fn parse_uptime_secs(raw: &str) -> Option<u64> {
    let secs = raw.split_whitespace().next()?.parse::<f64>().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(secs as u64)
}

/// Host uptime in whole seconds, or `None` when `/proc/uptime` is unavailable
/// or unparseable.
///
/// Deliberately `Option` rather than a `0` fallback: on macOS (where the daemon's
/// cross-platform modules are unit-tested) there is no `/proc`, and publishing
/// `0` would read as "this box just booted" instead of "we don't know".
pub fn uptime_seconds() -> Option<u64> {
    parse_uptime_secs(&fs::read_to_string("/proc/uptime").ok()?)
}

/// Read system uptime from `/proc/uptime` and format it as `Xd Xh Xm Xs`.
fn uptime_string() -> String {
    format_uptime(uptime_seconds().unwrap_or(0))
}

/// Build the `sys-status` JSON response.
pub fn sys_status_json() -> String {
    let os = serde_json::Value::String(os_name());
    let kernel = serde_json::Value::String(kernel_version());
    let host = serde_json::Value::String(hostname());
    let uptime = serde_json::Value::String(uptime_string());
    // Construct the object manually to guarantee field order and avoid an extra struct.
    format!(
        r#"{{"os":{},"kernel":{},"hostname":{},"uptime":{}}}"#,
        os, kernel, host, uptime
    )
}

// ---------------------------------------------------------------------------
// storage-status
// ---------------------------------------------------------------------------

/// One filesystem mount entry for `storage-status`.
#[derive(Debug, serde::Serialize)]
pub struct MountEntry {
    pub mount: String,
    /// Total size in bytes.
    pub size: u64,
    /// Used bytes.
    pub used: u64,
    /// Available (free) bytes.
    pub avail: u64,
    /// Usage percentage 0..=100.
    pub pct: u8,
}

/// Parse `/proc/mounts` and collect real filesystem mount points, then read
/// statvfs for each to get usage. Skips pseudo-filesystems (procfs, sysfs,
/// devtmpfs, tmpfs on /run/user/*, cgroup, etc.) using a small deny-list of
/// fs-types and mount-point prefixes that are never interesting to users.
///
/// This mirrors what `df -h` shows but returns raw bytes — the QML layer can
/// format them however it likes.
pub fn storage_status_json() -> String {
    let mounts = collect_mounts();
    serde_json::to_string(&mounts).unwrap_or_else(|_| "[]".into())
}

fn collect_mounts() -> Vec<MountEntry> {
    let content = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut seen_devices: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut entries: Vec<MountEntry> = Vec::new();

    for line in content.lines() {
        let parts: Vec<&str> = line.splitn(6, ' ').collect();
        if parts.len() < 4 {
            continue;
        }
        let device = parts[0];
        let mountpoint = parts[1];
        let fstype = parts[2];

        // Skip pseudo / kernel / user-specific filesystems.
        if is_skipped_fstype(fstype) || is_skipped_mountpoint(mountpoint) {
            continue;
        }

        // De-duplicate by device (e.g. the same disk mounted for bind in multiple places).
        if device.starts_with('/') && !seen_devices.insert(device.to_string()) {
            continue;
        }

        if let Some(entry) = statvfs_entry(mountpoint) {
            entries.push(entry);
        }
    }
    entries
}

fn is_skipped_fstype(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "bpf"
            | "tracefs"
            | "debugfs"
            | "securityfs"
            | "selinuxfs"
            | "fusectl"
            | "efivarfs"
            | "mqueue"
            | "hugetlbfs"
            | "nsfs"
            | "ramfs"
            | "overlay"
            | "aufs"
    )
}

fn is_skipped_mountpoint(mountpoint: &str) -> bool {
    mountpoint.starts_with("/proc")
        || mountpoint.starts_with("/sys")
        || mountpoint.starts_with("/dev/pts")
        || mountpoint.starts_with("/run/user")
        || mountpoint.starts_with("/run/snapd")
        || mountpoint == "/dev"
}

/// Call `statvfs(2)` on `mountpoint` and return a `MountEntry` with raw-byte
/// sizes. Returns `None` if the call fails (mount disappeared mid-read, etc.).
fn statvfs_entry(mountpoint: &str) -> Option<MountEntry> {
    use std::mem::MaybeUninit;

    let path = std::ffi::CString::new(mountpoint).ok()?;
    let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();

    // SAFETY: path is a valid NUL-terminated string; stat is written before use.
    let rc = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs succeeded, so stat is fully initialised.
    let stat = unsafe { stat.assume_init() };

    // f_bsize/f_blocks/f_bavail/f_bfree are u32 on Linux and u64 on macOS;
    // cast uniformly to u64 so arithmetic compiles on both platforms.
    #[allow(clippy::unnecessary_cast)]
    let bsize = stat.f_bsize as u64;
    #[allow(clippy::unnecessary_cast)]
    let size = stat.f_blocks as u64 * bsize;
    #[allow(clippy::unnecessary_cast)]
    let avail = stat.f_bavail as u64 * bsize;
    #[allow(clippy::unnecessary_cast)]
    let used = size.saturating_sub(stat.f_bfree as u64 * bsize);
    let pct = if size > 0 {
        ((used as f64 / size as f64) * 100.0).round() as u8
    } else {
        0
    };

    // Skip zero-size mounts (e.g. bind mounts of /dev entries).
    if size == 0 {
        return None;
    }

    // Verify the mountpoint still exists before reporting it.
    if !Path::new(mountpoint).exists() {
        return None;
    }

    Some(MountEntry {
        mount: mountpoint.to_string(),
        size,
        used,
        avail,
        pct,
    })
}

// ---------------------------------------------------------------------------
// sys-metrics (#235)
// ---------------------------------------------------------------------------

/// One temperature sensor reading for `sys-metrics`.
#[derive(Debug, serde::Serialize)]
pub struct TempEntry {
    /// Friendly label, e.g. `"CPU Tctl"`, `"GPU edge"`, `"NVMe Composite"`.
    pub label: String,
    /// Temperature in degrees Celsius, rounded to one decimal.
    pub celsius: f64,
}

/// Live hardware telemetry for the System page (#235). All fields degrade
/// gracefully to zero / empty on non-Linux hosts (no `/proc` or `/sys`).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SysMetrics {
    /// Aggregate CPU utilisation 0..=100 (sampled over a short window).
    pub cpu_pct: f64,
    /// Used memory in bytes (`MemTotal - MemAvailable`).
    pub mem_used: u64,
    /// Total memory in bytes.
    pub mem_total: u64,
    /// Memory usage percentage 0..=100.
    pub mem_pct: u8,
    /// 1-minute load average.
    pub load1: f64,
    /// Temperature sensors, CPU/GPU sorted first.
    pub temps: Vec<TempEntry>,
}

/// Read `(idle, total)` CPU jiffies from the aggregate `cpu` line of
/// `/proc/stat`. `idle` folds in `iowait`. Returns `None` if unparseable.
fn read_cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1) // skip the "cpu" label
        .filter_map(|s| s.parse().ok())
        .collect();
    if vals.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal guest guest_nice
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    let total: u64 = vals.iter().sum();
    Some((idle, total))
}

/// Aggregate CPU utilisation as a percentage, sampled over a ~200 ms window.
/// Two `/proc/stat` reads are required to compute a busy delta; the short
/// sleep is fine because this runs on tokio's blocking pool. Returns `0.0`
/// when `/proc/stat` is unavailable (non-Linux).
fn cpu_percent() -> f64 {
    let (idle1, total1) = match read_cpu_times() {
        Some(v) => v,
        None => return 0.0,
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (idle2, total2) = match read_cpu_times() {
        Some(v) => v,
        None => return 0.0,
    };
    let dt = total2.saturating_sub(total1);
    let di = idle2.saturating_sub(idle1);
    if dt == 0 {
        return 0.0;
    }
    (((dt - di) as f64 / dt as f64) * 100.0).clamp(0.0, 100.0)
}

/// Parse the leading kB number from a `/proc/meminfo` value (e.g.
/// `"  32768 kB"`) and return it in bytes.
fn parse_meminfo_kb(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// Read `(used, total)` memory in bytes from `/proc/meminfo`. `used` is
/// `MemTotal - MemAvailable` (matching what `free`/most tools report as used).
fn mem_info() -> (u64, u64) {
    let content = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse_meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = parse_meminfo_kb(rest);
        }
    }
    (total.saturating_sub(avail), total)
}

/// Map a raw hwmon chip name (+ optional per-sensor label) to a friendly,
/// couch-readable label. `k10temp`/`coretemp` → `CPU`, `amdgpu`/`nvidia` →
/// `GPU`, `nvme` → `NVMe`; everything else passes through verbatim.
fn friendly_temp_label(chip: &str, sub: Option<&str>) -> String {
    let base = match chip {
        "k10temp" | "coretemp" | "zenpower" | "cpu_thermal" => "CPU",
        "amdgpu" | "nouveau" | "nvidia" | "radeon" => "GPU",
        "nvme" => "NVMe",
        other => other,
    };
    match sub {
        Some(s) if !s.is_empty() && s != base => format!("{base} {s}"),
        _ => base.to_string(),
    }
}

/// Scan `/sys/class/hwmon/*/temp*_input` for temperature sensors. Each input
/// is reported in millidegrees; values outside (0, 150] °C are treated as
/// bogus and skipped. The list is bounded to keep the readout couch-sized.
fn read_temps() -> Vec<TempEntry> {
    let mut out: Vec<TempEntry> = Vec::new();
    let dir = match fs::read_dir("/sys/class/hwmon") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let chip = fs::read_to_string(path.join("name"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut inputs: Vec<String> = match fs::read_dir(&path) {
            Ok(files) => files
                .flatten()
                .filter_map(|f| f.file_name().into_string().ok())
                .filter(|n| n.starts_with("temp") && n.ends_with("_input"))
                .collect(),
            Err(_) => continue,
        };
        inputs.sort();
        for input in inputs {
            let raw = match fs::read_to_string(path.join(&input)) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let milli: f64 = match raw.trim().parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let celsius = milli / 1000.0;
            if celsius <= 0.0 || celsius > 150.0 {
                continue;
            }
            let label_file = input.replace("_input", "_label");
            let sub = fs::read_to_string(path.join(&label_file))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            out.push(TempEntry {
                label: friendly_temp_label(&chip, sub.as_deref()),
                celsius: (celsius * 10.0).round() / 10.0,
            });
        }
    }
    // CPU/GPU first, then everything else; bound the total.
    out.sort_by_key(|t| {
        if t.label.starts_with("CPU") {
            0
        } else if t.label.starts_with("GPU") {
            1
        } else {
            2
        }
    });
    out.truncate(8);
    out
}

/// Sample live hardware telemetry into a [`SysMetrics`] struct (#235).
///
/// Blocking: `cpu_percent()` sleeps ~200ms to compute a busy delta, so callers
/// on a tokio runtime must invoke this on the blocking pool. Reused by both the
/// `sys-metrics` IPC JSON response and the Prometheus metrics renderer
/// (`metrics::render_blocking`), so the two never drift.
pub fn sys_metrics() -> SysMetrics {
    let cpu_pct = (cpu_percent() * 10.0).round() / 10.0;
    let (mem_used, mem_total) = mem_info();
    let mem_pct = if mem_total > 0 {
        ((mem_used as f64 / mem_total as f64) * 100.0).round() as u8
    } else {
        0
    };
    let load1 = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .and_then(|v| v.parse::<f64>().ok())
        .map(|f| (f * 100.0).round() / 100.0)
        .unwrap_or(0.0);
    SysMetrics {
        cpu_pct,
        mem_used,
        mem_total,
        mem_pct,
        load1,
        temps: read_temps(),
    }
}

/// Build the `sys-metrics` JSON response (#235).
pub fn sys_metrics_json() -> String {
    serde_json::to_string(&sys_metrics()).unwrap_or_else(|_| "{}".into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_secs_parses_first_field_only() {
        // The real /proc/uptime shape: "<up> <idle>\n" — only the first field.
        assert_eq!(parse_uptime_secs("12345.67 98765.43\n"), Some(12345));
        assert_eq!(parse_uptime_secs("0.00 0.00\n"), Some(0));
        // A bare value with no idle field still parses.
        assert_eq!(parse_uptime_secs("42.9"), Some(42));
    }

    #[test]
    fn uptime_secs_returns_none_on_malformed_input() {
        // Honest "unknown" rather than a 0 that reads as "just booted".
        assert_eq!(parse_uptime_secs(""), None);
        assert_eq!(parse_uptime_secs("\n"), None);
        assert_eq!(parse_uptime_secs("not-a-number 1.0"), None);
        // A negative uptime is nonsense; `as u64` would wrap it huge.
        assert_eq!(parse_uptime_secs("-1.0 0.0"), None);
        assert_eq!(parse_uptime_secs("NaN 0.0"), None);
    }

    #[test]
    fn uptime_formats_seconds_only() {
        // 90 seconds = 1m 30s — exercises the real formatter.
        assert_eq!(format_uptime(90), "1m 30s");
    }

    #[test]
    fn uptime_formats_with_days() {
        // 2 days + 3 hours + 5 min + 10 sec — exercises the real formatter.
        let secs = 2 * 86400 + 3 * 3600 + 5 * 60 + 10;
        assert_eq!(format_uptime(secs), "2d 3h 5m 10s");
    }

    #[test]
    fn uptime_elides_zero_units() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(3600), "1h 0m 0s");
    }

    #[test]
    fn sys_status_json_is_valid_json_with_required_keys() {
        let json = sys_status_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(v.get("os").is_some(), "missing 'os' field");
        assert!(v.get("kernel").is_some(), "missing 'kernel' field");
        assert!(v.get("hostname").is_some(), "missing 'hostname' field");
        assert!(v.get("uptime").is_some(), "missing 'uptime' field");
    }

    #[test]
    fn skipped_fstypes_match_pseudo_filesystems() {
        assert!(is_skipped_fstype("proc"));
        assert!(is_skipped_fstype("sysfs"));
        assert!(is_skipped_fstype("devtmpfs"));
        assert!(is_skipped_fstype("cgroup2"));
        assert!(
            is_skipped_fstype("tmpfs"),
            "tmpfs must be skipped (covers /dev/shm, /run, /tmp)"
        );
        assert!(!is_skipped_fstype("ext4"));
        assert!(!is_skipped_fstype("btrfs"));
        assert!(!is_skipped_fstype("xfs"));
    }

    #[test]
    fn skipped_mountpoints_match_system_paths() {
        assert!(is_skipped_mountpoint("/proc/self/mnt"));
        assert!(is_skipped_mountpoint("/sys/fs/cgroup"));
        assert!(is_skipped_mountpoint("/run/user/1000"));
        assert!(!is_skipped_mountpoint("/home"));
        assert!(!is_skipped_mountpoint("/mnt/data"));
    }

    #[test]
    fn storage_status_json_is_valid_json_array() {
        // On any host (Linux or macOS) this returns a valid JSON array,
        // even if it's empty (non-Linux hosts won't have /proc/mounts).
        let json = storage_status_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(v.is_array(), "storage-status must be a JSON array");
    }

    #[test]
    fn parse_meminfo_kb_converts_to_bytes() {
        assert_eq!(parse_meminfo_kb("  32768 kB"), 32768 * 1024);
        assert_eq!(parse_meminfo_kb("0 kB"), 0);
        assert_eq!(parse_meminfo_kb("garbage"), 0);
    }

    #[test]
    fn friendly_temp_label_maps_known_chips() {
        assert_eq!(friendly_temp_label("k10temp", Some("Tctl")), "CPU Tctl");
        assert_eq!(friendly_temp_label("amdgpu", Some("edge")), "GPU edge");
        assert_eq!(
            friendly_temp_label("nvme", Some("Composite")),
            "NVMe Composite"
        );
        // No sub-label → bare base name (no trailing space).
        assert_eq!(friendly_temp_label("coretemp", None), "CPU");
        // A sub-label equal to the base is not duplicated.
        assert_eq!(friendly_temp_label("k10temp", Some("CPU")), "CPU");
        // Unknown chip passes through.
        assert_eq!(friendly_temp_label("iwlwifi", None), "iwlwifi");
    }

    #[test]
    fn sys_metrics_json_is_valid_json_with_required_keys() {
        let json = sys_metrics_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        for key in ["cpuPct", "memUsed", "memTotal", "memPct", "load1", "temps"] {
            assert!(v.get(key).is_some(), "missing '{key}' field");
        }
        assert!(v["temps"].is_array(), "temps must be a JSON array");
        // mem_pct is always in range even on a non-Linux host (where it's 0).
        assert!(v["memPct"].as_u64().unwrap() <= 100);
    }
}
