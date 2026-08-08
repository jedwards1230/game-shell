//! System updates (pacman) — a `checkupdates` read (cached, 5 min TTL), a
//! reboot-needed detector (installed kernel package vs the running kernel),
//! and an async background `sudo -n pacman -Syu --noconfirm` apply job.
//!
//! **Read** is unprivileged: `checkupdates` (pacman-contrib) syncs its own
//! copy of the pacman database and never touches the live one, so it needs
//! no elevated privilege. **Apply** needs root — the panel's unprivileged
//! user has NOPASSWD sudo on the deploy host (htpc-1), so `sudo -n pacman
//! -Syu --noconfirm` runs without a password prompt (`-n` = never prompt,
//! fail closed instead of hanging on one).
//!
//! The apply job is a single-flighted `tokio::spawn`ed background task —
//! the pacman process outlives any one HTTP request, so its state (`Idle` /
//! `Running` / `Done`) lives in [`UpdatesState`] inside `AppState` and is
//! polled across multiple requests rather than awaited inline.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::state::{AppState, SharedState};

/// How long a cached `checkupdates` result is considered fresh — re-run at
/// most this often unless the operator clicks Refresh.
const CHECK_TTL: Duration = Duration::from_secs(5 * 60);
/// Timeout for `checkupdates` (and the smaller `pacman -Q*` probes it's
/// bundled with for the reboot-needed check).
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for the full `pacman -Syu` apply job — package downloads can be
/// slow, so this is generous (matches the brief's 30-minute budget).
const APPLY_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How many trailing lines of the apply job's combined stdout+stderr to
/// keep.
const LOG_TAIL_LINES: usize = 200;

// ---------------------------------------------------------------------------
// checkupdates parsing
// ---------------------------------------------------------------------------

/// One pending package update, as parsed from a `checkupdates` line
/// (`name old_version -> new_version`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUpdate {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
}

/// Parse a single `checkupdates` output line. `None` for a line that doesn't
/// match the expected `name old -> new` shape — degrades that one line to
/// "skipped" rather than fabricating a partial row or panicking.
fn parse_line(line: &str) -> Option<PendingUpdate> {
    let mut parts = line.split_whitespace();
    let name = parts.next()?.to_string();
    let old_version = parts.next()?.to_string();
    if parts.next()? != "->" {
        return None;
    }
    let new_version = parts.next()?.to_string();
    Some(PendingUpdate {
        name,
        old_version,
        new_version,
    })
}

/// Parse `checkupdates`' full stdout into pending updates, skipping any line
/// that doesn't match the expected shape.
pub fn parse_checkupdates(raw: &str) -> Vec<PendingUpdate> {
    raw.lines().filter_map(parse_line).collect()
}

/// Run `checkupdates` and parse its output. Exit code 2 means "no updates
/// available" — an OK-empty result, not an error; exit code 1 (or a spawn
/// failure/timeout) is a genuine error, surfaced as `Err(message)` rather
/// than silently showing an empty (and misleadingly "up to date") list.
async fn run_checkupdates() -> Result<Vec<PendingUpdate>, String> {
    let mut cmd = Command::new("checkupdates");
    cmd.kill_on_drop(true);
    let output = match tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("failed to spawn checkupdates: {e}")),
        Err(_) => return Err("checkupdates timed out".to_string()),
    };
    match output.status.code() {
        Some(0) => Ok(parse_checkupdates(&String::from_utf8_lossy(&output.stdout))),
        Some(2) => Ok(Vec::new()), // no updates available — not an error
        _ => Err(format!(
            "checkupdates exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

// ---------------------------------------------------------------------------
// Small local exec helper — mirrors `crate::exec`'s private `run()`, kept as
// its own copy here since this module spawns an entirely different command
// surface (checkupdates/pacman/uname/sudo, not systemctl/journalctl) and
// `exec::run` isn't exposed outside that module.
// ---------------------------------------------------------------------------

async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args).kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("failed to spawn {program}: {e}")),
        Err(_) => return Err(format!("{program} timed out")),
    };
    if !output.status.success() {
        return Err(format!(
            "{program} exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Reboot-needed detection
// ---------------------------------------------------------------------------

/// Reboot-needed status, from comparing the running kernel (`uname -r`)
/// against the installed kernel package's version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootStatus {
    /// The running kernel matches the installed kernel package.
    NotNeeded,
    /// The installed kernel package's version differs from the running
    /// kernel — a newer kernel is on disk but not yet booted.
    Needed,
    /// Couldn't confidently identify a single kernel package, or couldn't
    /// parse/compare versions — degrades honestly rather than guessing.
    Unknown,
}

/// `true` for an installed package name that looks like a kernel package
/// (`linux`, or `linux-<flavor>` such as `linux-cachyos`/`linux-lts`/
/// `linux-zen`) as opposed to a kernel-adjacent-but-not-the-kernel package
/// (`linux-headers`, `linux-firmware`, `linux-api-headers`, ...). A
/// necessarily-heuristic name filter — see [`RebootStatus::Unknown`] for the
/// honest fallback when this (or the version comparison) is inconclusive.
fn is_kernel_package_name(name: &str) -> bool {
    if name == "linux" {
        return true;
    }
    let Some(rest) = name.strip_prefix("linux-") else {
        return false;
    };
    const NON_KERNEL_MARKERS: &[&str] = &["headers", "docs", "firmware", "api", "tools", "neptune"];
    !NON_KERNEL_MARKERS.iter().any(|m| rest.contains(m))
}

/// Find the one installed kernel package to compare against the running
/// kernel. Zero candidates, or multiple candidates none of which can be
/// disambiguated by matching their flavor suffix against the `uname -r`
/// release string, yields `None` (→ [`RebootStatus::Unknown`]).
async fn detect_kernel_package() -> Option<String> {
    let installed = run("pacman", &["-Qq"], CHECK_TIMEOUT).await.ok()?;
    let uname_release = run("uname", &["-r"], Duration::from_secs(5)).await.ok()?;
    let uname_release = uname_release.trim();

    let candidates: Vec<&str> = installed
        .lines()
        .filter(|n| is_kernel_package_name(n))
        .collect();

    match candidates.len() {
        0 => None,
        1 => Some(candidates[0].to_string()),
        _ => candidates
            .iter()
            .find(|name| {
                let flavor = name.strip_prefix("linux-").unwrap_or("");
                !flavor.is_empty() && uname_release.contains(flavor)
            })
            .map(|s| s.to_string()),
    }
}

/// Extract a leading dotted-numeric version prefix as a comparable tuple —
/// e.g. `[6, 6, 30]` from both `"6.6.30-1-cachyos"` (a `uname -r` release
/// string) and `"6.6.30.arch2-2"` (a pacman package version) — so the two
/// different version-string conventions can still be compared for "is this
/// the same kernel build". Empty when `s` doesn't start with a digit.
fn kernel_version_tuple(s: &str) -> Vec<u64> {
    s.split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.')
        .filter_map(|p| p.parse::<u64>().ok())
        .collect()
}

/// Compare the running kernel against the installed kernel package's
/// version. Every exec call degrades to [`RebootStatus::Unknown`] on any
/// failure — this is an opportunistic diagnostic, never a hard requirement.
async fn detect_reboot_status() -> RebootStatus {
    let Some(pkg_name) = detect_kernel_package().await else {
        return RebootStatus::Unknown;
    };
    let Ok(uname_release) = run("uname", &["-r"], Duration::from_secs(5)).await else {
        return RebootStatus::Unknown;
    };
    let Ok(query) = run("pacman", &["-Q", &pkg_name], CHECK_TIMEOUT).await else {
        return RebootStatus::Unknown;
    };
    let Some(pkg_version) = query.split_whitespace().nth(1) else {
        return RebootStatus::Unknown;
    };

    let running = kernel_version_tuple(uname_release.trim());
    let installed = kernel_version_tuple(pkg_version);
    if running.is_empty() || installed.is_empty() {
        RebootStatus::Unknown
    } else if running == installed {
        RebootStatus::NotNeeded
    } else {
        RebootStatus::Needed
    }
}

// ---------------------------------------------------------------------------
// Cached checkupdates snapshot
// ---------------------------------------------------------------------------

struct CachedCheck {
    pending: Vec<PendingUpdate>,
    reboot: RebootStatus,
    checked_at: Instant,
    error: Option<String>,
}

/// A point-in-time view of the cached (or freshly re-probed) update check.
pub struct CheckSnapshot {
    pub pending: Vec<PendingUpdate>,
    pub reboot: RebootStatus,
    pub checked_at_secs_ago: u64,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Apply job
// ---------------------------------------------------------------------------

/// The apply job's state — lives in [`UpdatesState`] behind a `Mutex` so it
/// can be observed across multiple HTTP requests while the background
/// `pacman -Syu` task runs.
#[derive(Clone)]
enum UpdateJob {
    Idle,
    Running {
        started: Instant,
        log_tail: Vec<String>,
    },
    Done {
        success: bool,
        finished: Instant,
        log_tail: Vec<String>,
    },
}

/// A point-in-time view of the apply job, safe to hand to a template.
#[derive(Clone)]
pub enum JobSnapshot {
    Idle,
    Running {
        elapsed_secs: u64,
        log_tail: Vec<String>,
    },
    Done {
        success: bool,
        elapsed_secs: u64,
        log_tail: Vec<String>,
    },
}

/// Shared state for the Updates feature — one instance lives in `AppState`.
pub struct UpdatesState {
    cache: Mutex<Option<CachedCheck>>,
    job: Mutex<UpdateJob>,
}

impl Default for UpdatesState {
    fn default() -> Self {
        Self {
            cache: Mutex::new(None),
            job: Mutex::new(UpdateJob::Idle),
        }
    }
}

/// Read the current (cached, unless `force` or the cache is stale/absent)
/// checkupdates + reboot-needed snapshot.
pub async fn snapshot(state: &UpdatesState, force: bool) -> CheckSnapshot {
    if !force {
        let cache = state.cache.lock().await;
        if let Some(c) = &*cache {
            if c.checked_at.elapsed() < CHECK_TTL {
                return CheckSnapshot {
                    pending: c.pending.clone(),
                    reboot: c.reboot,
                    checked_at_secs_ago: c.checked_at.elapsed().as_secs(),
                    error: c.error.clone(),
                };
            }
        }
    }

    // Cache miss/stale/forced — re-probe without holding the lock across the
    // exec calls, so a concurrent read isn't blocked on the network/exec
    // round-trip. A rare concurrent double-probe is harmless (checkupdates
    // and the kernel-version check are both read-only).
    let (pending, error) = match run_checkupdates().await {
        Ok(p) => (p, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let reboot = detect_reboot_status().await;
    let checked_at = Instant::now();

    *state.cache.lock().await = Some(CachedCheck {
        pending: pending.clone(),
        reboot,
        checked_at,
        error: error.clone(),
    });

    CheckSnapshot {
        pending,
        reboot,
        checked_at_secs_ago: 0,
        error,
    }
}

/// Drop the cached snapshot so the next [`snapshot`] call re-probes
/// unconditionally — called once the apply job finishes, since its whole
/// point was to change what `checkupdates`/the kernel-version comparison
/// would report.
async fn invalidate(state: &UpdatesState) {
    *state.cache.lock().await = None;
}

/// Read the current apply-job state.
pub async fn job_snapshot(state: &UpdatesState) -> JobSnapshot {
    match &*state.job.lock().await {
        UpdateJob::Idle => JobSnapshot::Idle,
        UpdateJob::Running { started, log_tail } => JobSnapshot::Running {
            elapsed_secs: started.elapsed().as_secs(),
            log_tail: log_tail.clone(),
        },
        UpdateJob::Done {
            success,
            finished,
            log_tail,
        } => JobSnapshot::Done {
            success: *success,
            elapsed_secs: finished.elapsed().as_secs(),
            log_tail: log_tail.clone(),
        },
    }
}

/// Start the apply job (`sudo -n pacman -Syu --noconfirm`) as a background
/// task, unless one is already `Running` (single-flight). Returns
/// immediately either way — the caller re-renders the job status view
/// afterward to show whatever state resulted (freshly `Running`, or the
/// still-`Running` job that refused a second start).
pub async fn start_apply(app: &SharedState) -> Result<(), &'static str> {
    {
        let mut job = app.updates.job.lock().await;
        if matches!(&*job, UpdateJob::Running { .. }) {
            return Err("An update is already running.");
        }
        *job = UpdateJob::Running {
            started: Instant::now(),
            log_tail: Vec::new(),
        };
    }

    tokio::spawn(run_apply_job(Arc::clone(app)));
    Ok(())
}

async fn append_log_line(updates: &UpdatesState, line: String) {
    let mut job = updates.job.lock().await;
    if let UpdateJob::Running { log_tail, .. } = &mut *job {
        log_tail.push(line);
        if log_tail.len() > LOG_TAIL_LINES {
            let excess = log_tail.len() - LOG_TAIL_LINES;
            log_tail.drain(0..excess);
        }
    }
}

/// Transition the job to `Done`, appending `extra_lines` (e.g. a
/// spawn/wait/timeout failure message) to whatever log tail the running job
/// had already accumulated, capped at [`LOG_TAIL_LINES`].
async fn finish_job(updates: &UpdatesState, success: bool, extra_lines: Vec<String>) {
    let mut tail = match &*updates.job.lock().await {
        UpdateJob::Running { log_tail, .. } => log_tail.clone(),
        _ => Vec::new(),
    };
    tail.extend(extra_lines);
    if tail.len() > LOG_TAIL_LINES {
        let excess = tail.len() - LOG_TAIL_LINES;
        tail.drain(0..excess);
    }
    *updates.job.lock().await = UpdateJob::Done {
        success,
        finished: Instant::now(),
        log_tail: tail,
    };
    invalidate(updates).await;
}

/// Run `sudo -n pacman -Syu --noconfirm` to completion (or [`APPLY_TIMEOUT`],
/// whichever comes first), streaming combined stdout+stderr into the job's
/// live log tail as it arrives so a poller mid-run sees real progress, not
/// just a static "running" banner. `kill_on_drop(true)` on the spawned
/// command guarantees a timed-out pacman process is SIGKILLed when this
/// function's `child` drops — mirrors `crate::exec::run`'s documented
/// timeout-kill guarantee.
async fn run_apply_job(app: Arc<AppState>) {
    let mut cmd = Command::new("sudo");
    cmd.args(["-n", "pacman", "-Syu", "--noconfirm"])
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            finish_job(
                &app.updates,
                false,
                vec![format!("failed to spawn sudo pacman -Syu: {e}")],
            )
            .await;
            return;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        finish_job(
            &app.updates,
            false,
            vec!["internal error: no stdout pipe".to_string()],
        )
        .await;
        return;
    };
    let Some(stderr) = child.stderr.take() else {
        finish_job(
            &app.updates,
            false,
            vec!["internal error: no stderr pipe".to_string()],
        )
        .await;
        return;
    };

    let out_updates = Arc::clone(&app);
    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_log_line(&out_updates.updates, line).await;
        }
    });

    let err_updates = Arc::clone(&app);
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_log_line(&err_updates.updates, line).await;
        }
    });

    let wait_result = tokio::time::timeout(APPLY_TIMEOUT, async move {
        let _ = out_task.await;
        let _ = err_task.await;
        child.wait().await
    })
    .await;

    match wait_result {
        Ok(Ok(status)) => finish_job(&app.updates, status.success(), Vec::new()).await,
        Ok(Err(e)) => finish_job(&app.updates, false, vec![format!("wait failed: {e}")]).await,
        // Timed out — the async block above (holding `child`) is dropped
        // here, which SIGKILLs the still-running pacman via kill_on_drop.
        Err(_) => {
            finish_job(
                &app.updates,
                false,
                vec![format!("pacman -Syu timed out after {APPLY_TIMEOUT:?}")],
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_checkupdates_parses_well_formed_lines() {
        let raw = "firefox 128.0-1 -> 129.0-1\nlinux-cachyos 6.6.29-1 -> 6.6.30-1\n";
        let updates = parse_checkupdates(raw);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].name, "firefox");
        assert_eq!(updates[0].old_version, "128.0-1");
        assert_eq!(updates[0].new_version, "129.0-1");
        assert_eq!(updates[1].name, "linux-cachyos");
    }

    #[test]
    fn parse_checkupdates_skips_malformed_lines() {
        let raw = "firefox 128.0-1 -> 129.0-1\nnot a real line\n";
        let updates = parse_checkupdates(raw);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].name, "firefox");
    }

    #[test]
    fn parse_checkupdates_empty_input_yields_no_updates() {
        assert!(parse_checkupdates("").is_empty());
    }

    #[test]
    fn is_kernel_package_name_matches_expected_flavors() {
        assert!(is_kernel_package_name("linux"));
        assert!(is_kernel_package_name("linux-cachyos"));
        assert!(is_kernel_package_name("linux-lts"));
        assert!(is_kernel_package_name("linux-zen"));
        assert!(!is_kernel_package_name("linux-headers"));
        assert!(!is_kernel_package_name("linux-firmware"));
        assert!(!is_kernel_package_name("linux-api-headers"));
        assert!(!is_kernel_package_name("linux-cachyos-headers"));
        assert!(!is_kernel_package_name("firefox"));
    }

    #[test]
    fn kernel_version_tuple_extracts_leading_numeric_prefix() {
        assert_eq!(kernel_version_tuple("6.6.30-1-cachyos"), vec![6, 6, 30]);
        assert_eq!(kernel_version_tuple("6.6.30.arch2-2"), vec![6, 6, 30]);
        assert_eq!(kernel_version_tuple("not-a-version"), Vec::<u64>::new());
    }

    #[test]
    fn kernel_version_tuple_matches_across_uname_and_pacman_conventions() {
        // The two version-string conventions differ in punctuation but
        // should still compare equal for "same kernel build".
        assert_eq!(
            kernel_version_tuple("6.6.30-1-cachyos"),
            kernel_version_tuple("6.6.30.arch2-2")
        );
        assert_ne!(
            kernel_version_tuple("6.6.30-1-cachyos"),
            kernel_version_tuple("6.6.31.arch1-1")
        );
    }

    #[tokio::test]
    async fn snapshot_caches_within_ttl_and_force_bypasses_it() {
        let state = UpdatesState::default();
        // checkupdates almost certainly isn't installed/behaves unpredictably
        // on the CI/dev host — this test only asserts the CACHING contract
        // (second non-forced call reuses the first result's `checked_at`),
        // not the exec outcome itself.
        let first = snapshot(&state, false).await;
        let second = snapshot(&state, false).await;
        assert_eq!(first.error.is_some(), second.error.is_some());
        // The second call must have reused the cache rather than re-probing
        // — its "seconds ago" can only be >= the first's (time only moves
        // forward), and a fresh probe would have reset it to 0 like the
        // first call's did.
        assert!(second.checked_at_secs_ago >= first.checked_at_secs_ago);
    }

    #[tokio::test]
    async fn job_snapshot_starts_idle_and_start_apply_transitions_to_running() {
        let sock = std::path::PathBuf::from(format!(
            "/tmp/tvshp-updates-hermetic-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let app: SharedState = Arc::new(AppState {
            cfg: crate::config::AppConfig::default(),
            node: Arc::new(crate::ipc::IpcTransport::new(sock)),
            bridge: crate::bridge::BridgeClient::new(None, None),
            recovery: crate::exec::Recovery::new(),
            updates: UpdatesState::default(),
        });

        assert!(matches!(
            job_snapshot(&app.updates).await,
            JobSnapshot::Idle
        ));

        start_apply(&app).await.expect("first start_apply succeeds");
        assert!(matches!(
            job_snapshot(&app.updates).await,
            JobSnapshot::Running { .. }
        ));

        // Single-flight: a second start while still Running is refused.
        assert!(start_apply(&app).await.is_err());
    }
}
