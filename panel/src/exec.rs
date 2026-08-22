//! Direct-exec recovery tier: spawns local commands (systemctl/journalctl/
//! build script) when the daemon's IPC socket and HTTP bridge are both
//! unreachable. This is the panel's last-resort control surface.
//!
//! Every DESTRUCTIVE operation (restart/build/reboot/suspend) is
//! single-flighted behind one shared `tokio::sync::Mutex<()>` so concurrent
//! clicks from the Dev page can never race two restarts/builds/etc.
//! Non-destructive reads (journal tail, unit status) do NOT take the lock.
//!
//! Cross-platform-compilable: these are just `tokio::process::Command`
//! spawns, so the crate builds on macOS even though the commands
//! (`systemctl`, `journalctl`) only make sense on the Linux deploy target.

use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

/// Timeout for the build script (matches the daemon bridge's own dev-op
/// timeout budget).
const BUILD_TIMEOUT: Duration = Duration::from_secs(180);
/// Timeout for systemctl restart/reboot/suspend calls — these should return
/// almost immediately (systemd hands off the restart asynchronously).
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for journalctl / is-active reads.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors a local command spawn/run can produce.
#[derive(Debug)]
pub enum ExecError {
    /// The command could not be spawned (binary not found, permissions, ...).
    Spawn(String),
    /// The command did not finish within its timeout.
    Timeout,
    /// The command exited non-zero; `i32` is the exit code (best-effort —
    /// `-1` when the process was terminated by a signal) and the `String` is
    /// the combined stdout+stderr.
    NonZero(i32, String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Spawn(msg) => write!(f, "failed to spawn command: {msg}"),
            ExecError::Timeout => write!(f, "command timed out"),
            ExecError::NonZero(code, body) => write!(f, "command exited {code}: {body}"),
        }
    }
}

impl std::error::Error for ExecError {}

/// Direct-exec recovery tier. Holds the single-flight lock for destructive
/// operations.
pub struct Recovery {
    lock: Mutex<()>,
}

impl Default for Recovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Recovery {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    // ── Destructive (single-flight) ─────────────────────────────────────

    /// `systemctl --user restart <daemon-unit>`.
    pub async fn restart_daemon(&self) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        run(
            "systemctl",
            &["--user", "restart", &crate::config::daemon_unit()],
            SYSTEMCTL_TIMEOUT,
        )
        .await
    }

    /// `systemctl --user restart <shell-unit>`.
    pub async fn restart_shell(&self) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        run(
            "systemctl",
            &["--user", "restart", &crate::config::shell_unit()],
            SYSTEMCTL_TIMEOUT,
        )
        .await
    }

    /// Run `scripts/build-daemon.sh`, resolved via `$TV_SHELL_DIR` else the
    /// default install root, else bare `build-daemon.sh` on `PATH`.
    pub async fn build_daemon(&self) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        let script = resolve_build_script();
        run(&script, &[], BUILD_TIMEOUT).await
    }

    /// `systemctl reboot`.
    pub async fn reboot(&self) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        run("systemctl", &["reboot"], SYSTEMCTL_TIMEOUT).await
    }

    /// `systemctl suspend`.
    pub async fn suspend(&self) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        run("systemctl", &["suspend"], SYSTEMCTL_TIMEOUT).await
    }

    /// `systemctl --user restart <unit>` for an arbitrary unit name — a
    /// generic counterpart to [`Self::restart_daemon`]/[`Self::restart_shell`]
    /// used by the Processes page, which restarts all three tv-shell units
    /// (daemon/shell/panel) from one code path rather than three near-
    /// identical named wrappers. The caller is responsible for only passing a
    /// known-good unit name (see `pages::services::render_restart`, which
    /// maps a fixed key to a unit name rather than accepting one from the
    /// client directly).
    pub async fn restart_unit(&self, unit: &str) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        run("systemctl", &["--user", "restart", unit], SYSTEMCTL_TIMEOUT).await
    }

    // ── Non-destructive (no lock) ────────────────────────────────────────

    /// `journalctl --user -u <unit> -n <lines> --no-pager`, then post-filter
    /// lines containing `filter` (substring match) when given.
    pub async fn journal_unit(
        &self,
        unit: &str,
        lines: usize,
        filter: Option<&str>,
    ) -> Result<String, ExecError> {
        let lines_str = lines.to_string();
        let out = run(
            "journalctl",
            &["--user", "-u", unit, "-n", &lines_str, "--no-pager"],
            READ_TIMEOUT,
        )
        .await?;
        Ok(apply_filter(out, filter))
    }

    /// `journalctl --user -t <tag> -n <lines> --no-pager`, then post-filter.
    ///
    /// Not yet called by the M1 Logs page (see
    /// [`crate::config::shell_journal_tag`]) — reserved for a future
    /// direct-exec shell-log fallback.
    #[allow(dead_code)]
    pub async fn journal_tag(
        &self,
        tag: &str,
        lines: usize,
        filter: Option<&str>,
    ) -> Result<String, ExecError> {
        let lines_str = lines.to_string();
        let out = run(
            "journalctl",
            &["--user", "-t", tag, "-n", &lines_str, "--no-pager"],
            READ_TIMEOUT,
        )
        .await?;
        Ok(apply_filter(out, filter))
    }

    /// `systemctl --user is-active <unit>`, trimmed. A spawn failure or
    /// timeout degrades to `"unknown"` rather than propagating an error —
    /// this is a status probe, not a control action.
    pub async fn unit_active(&self, unit: &str) -> String {
        match run("systemctl", &["--user", "is-active", unit], READ_TIMEOUT).await {
            Ok(out) => {
                let trimmed = out.trim();
                if trimmed.is_empty() {
                    "unknown".to_string()
                } else {
                    trimmed.to_string()
                }
            }
            // `systemctl is-active` exits non-zero (with the state as stdout,
            // e.g. "inactive"/"failed") when the unit isn't running — that's
            // still a meaningful state, so surface the body rather than
            // collapsing to "unknown".
            Err(ExecError::NonZero(_, body)) => {
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    "unknown".to_string()
                } else {
                    trimmed.lines().next().unwrap_or("unknown").to_string()
                }
            }
            Err(_) => "unknown".to_string(),
        }
    }

    /// Top ~15 processes by CPU: `ps axo pid,pcpu,pmem,comm --sort=-pcpu`
    /// (GNU `ps`), truncated to a header line + 15 rows. Read-only — no kill
    /// action in v1 (deferred; see `docs/PANEL.md`). Non-destructive — no
    /// lock.
    pub async fn top_processes(&self) -> Result<String, ExecError> {
        let out = run(
            "ps",
            &["axo", "pid,pcpu,pmem,comm", "--sort=-pcpu"],
            READ_TIMEOUT,
        )
        .await?;
        Ok(out.lines().take(16).collect::<Vec<_>>().join("\n"))
    }
}

/// Post-filter `output`'s lines by substring `filter`, if given.
fn apply_filter(output: String, filter: Option<&str>) -> String {
    match filter {
        Some(f) if !f.is_empty() => output
            .lines()
            .filter(|line| line.contains(f))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => output,
    }
}

/// Resolve `scripts/build-daemon.sh`: `$TV_SHELL_DIR/scripts/build-daemon.sh`
/// if `TV_SHELL_DIR` is set and exists, else
/// `<install_root_default>/scripts/build-daemon.sh` if it exists, else the
/// bare `build-daemon.sh` (resolved on `PATH` by the shell/exec machinery).
fn resolve_build_script() -> String {
    if let Some(dir) = tv_shell_protocol::brand::env("DIR") {
        let candidate = std::path::Path::new(&dir).join("scripts/build-daemon.sh");
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    let default_candidate =
        tv_shell_protocol::brand::install_root_default().join("scripts/build-daemon.sh");
    if default_candidate.exists() {
        return default_candidate.to_string_lossy().into_owned();
    }
    "build-daemon.sh".to_string()
}

/// Spawn `program args...`, wait up to `timeout`, and return combined
/// stdout+stderr on success or the appropriate [`ExecError`] otherwise.
///
/// `kill_on_drop` guarantees a timed-out child is SIGKILLed when the
/// `output()` future is dropped — without it the child would keep running
/// as an orphan past the timeout, letting a second invocation race it even
/// though the caller-side single-flight mutex has been released.
async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, ExecError> {
    let mut cmd = Command::new(program);
    cmd.args(args).kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(ExecError::Spawn(e.to_string())),
        Err(_) => return Err(ExecError::Timeout),
    };

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }

    if output.status.success() {
        Ok(combined)
    } else {
        let code = output.status.code().unwrap_or(-1);
        Err(ExecError::NonZero(code, combined))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unit_active_never_panics_and_returns_some_string() {
        let recovery = Recovery::new();
        let status = recovery
            .unit_active("definitely-not-a-real-unit.service")
            .await;
        assert!(
            !status.is_empty(),
            "unit_active must always return a non-empty string"
        );
    }

    #[test]
    fn apply_filter_keeps_matching_lines_only() {
        let out = "alpha line\nbeta line\ngamma alpha\n".to_string();
        let filtered = apply_filter(out, Some("alpha"));
        assert_eq!(filtered, "alpha line\ngamma alpha");
    }

    #[test]
    fn apply_filter_passthrough_when_none() {
        let out = "alpha\nbeta\n".to_string();
        assert_eq!(apply_filter(out.clone(), None), out);
    }

    #[tokio::test]
    async fn run_kills_child_on_timeout() {
        // Unique sleep duration so pgrep -f can find exactly this child.
        let marker = format!("4.{}9317", std::process::id() % 1000);
        let result = run("sleep", &[&marker], Duration::from_millis(100)).await;
        assert!(matches!(result, Err(ExecError::Timeout)));

        // kill_on_drop delivers SIGKILL when the output() future drops; give
        // the kernel a beat, then verify no orphan survived. Skip the
        // assertion if pgrep itself is unavailable on this system.
        let pattern = format!("sleep {marker}");
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            match std::process::Command::new("pgrep")
                .args(["-f", &pattern])
                .output()
            {
                Ok(out) if !out.status.success() => return, // no match: child is dead
                Ok(_) => continue,                          // still alive, keep polling
                Err(_) => return,                           // no pgrep — skip
            }
        }
        panic!("timed-out child was still alive 2s after the timeout");
    }
}
