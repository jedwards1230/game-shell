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

use crate::config::{RestartTarget, UnitName, UnitScope};

/// Timeout for the build script (matches the daemon bridge's own dev-op
/// timeout budget).
const BUILD_TIMEOUT: Duration = Duration::from_secs(180);
/// Timeout for systemctl restart/reboot/suspend calls — these should return
/// almost immediately (systemd hands off the restart asynchronously).
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for journalctl / is-active reads.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The `systemctl show` properties [`Recovery::show_unit`] asks for — exactly
/// what System ▸ Services renders and nothing else, so the output stays small
/// and the parser has a fixed vocabulary.
const SHOW_PROPERTIES: &str = "--property=Id,Description,LoadState,ActiveState,SubState,\
     UnitFileState,ActiveEnterTimestamp,Result,StatusText,ExecMainStatus,LoadError";

/// Resolve a built-in key to its [`RestartTarget`], panicking on an unknown
/// one — the three call sites pass literals, and `config`'s
/// `built_in_unit_names_are_valid_unit_names` pins that they resolve.
fn builtin(key: &'static str) -> RestartTarget {
    crate::config::builtin_target(key).expect("built-in unit key")
}

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
    /// `sudo -n` refused to elevate — no sudoers rule for this exact command,
    /// or none that is NOPASSWD. The `String` is sudo's own line.
    ///
    /// A distinct variant rather than a `NonZero` with a suggestive body,
    /// because the callers must not be able to render it as a generic failure:
    /// "not permitted on this node" and "the restart was attempted and failed"
    /// send an operator to two different places, and only one of them is true.
    /// See [`is_sudo_refusal`].
    NotPermitted(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Spawn(msg) => write!(f, "failed to spawn command: {msg}"),
            ExecError::Timeout => write!(f, "command timed out"),
            ExecError::NonZero(code, body) => write!(f, "command exited {code}: {body}"),
            ExecError::NotPermitted(detail) => {
                write!(f, "not permitted on this node: {detail}")
            }
        }
    }
}

/// What `sudo -n` prints when it will not elevate without a prompt.
///
/// Matched case-insensitively as substrings because the exact wording varies
/// across sudo versions and locales; every one of these means the same thing
/// operationally — **the rule is missing, nothing ran**.
const SUDO_REFUSAL_MARKERS: [&str; 6] = [
    "a password is required",
    "a terminal is required",
    "no tty present",
    "no askpass program",
    "is not allowed to execute",
    "may not run",
];

/// Whether a failed `sudo -n` output is a refusal-to-elevate (as opposed to
/// the elevated command itself having failed).
pub fn is_sudo_refusal(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    SUDO_REFUSAL_MARKERS.iter().any(|m| lower.contains(m))
}

/// Reclassify a `sudo -n` failure into [`ExecError::NotPermitted`] when it was
/// sudo, not the wrapped command, that said no.
///
/// A missing `sudo` binary counts: on a node with no sudo at all the elevated
/// action is exactly as unavailable, and reporting "failed to spawn command"
/// would send the operator looking at `systemctl`.
fn classify_sudo_failure(err: ExecError) -> ExecError {
    match err {
        ExecError::NonZero(_, body) if is_sudo_refusal(&body) => {
            ExecError::NotPermitted(body.trim().to_string())
        }
        ExecError::Spawn(msg) => ExecError::NotPermitted(format!("sudo is unavailable: {msg}")),
        other => other,
    }
}

impl std::error::Error for ExecError {}

/// Direct-exec recovery tier. Holds the single-flight lock for destructive
/// operations.
pub struct Recovery {
    lock: Mutex<()>,
    /// The elevation helper for system-scope restarts, as an argv PREFIX —
    /// always exactly `["sudo"]` in production, pinned by
    /// [`tests::the_production_elevation_prefix_is_exactly_sudo`].
    ///
    /// A test seam, so the fail-closed path can be exercised without a real
    /// `sudo` on the machine running the suite (and, more to the point,
    /// without a machine that has NOPASSWD ALL quietly turning the test into
    /// a real restart). It is a prefix rather than a bare program name so the
    /// fake can be reached *through* an interpreter — see
    /// [`Recovery::with_sudo`].
    sudo: Vec<String>,
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
            sudo: vec!["sudo".to_string()],
        }
    }

    /// Point the elevation prefix somewhere other than `sudo`. See
    /// [`Recovery::sudo`].
    ///
    /// Takes an argv PREFIX rather than a program name so a caller can pass
    /// `["sh", "<script>"]`. That is not a stylistic choice: a test that
    /// execs a script it just wrote races `ETXTBSY`. `fork` copies the
    /// still-open write fd into every concurrently spawning child, and the
    /// kernel refuses `execve` on a file any process holds open for writing —
    /// so the exec fails, intermittently, in proportion to how many other
    /// tests are spawning at that moment. Running the fake as `sh <script>`
    /// makes `execve` target `/bin/sh`, which no test wrote, and leaves the
    /// script itself only ever *read*. See tv-shell#428.
    #[cfg(test)]
    pub fn with_sudo<I, S>(prefix: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            lock: Mutex::new(()),
            sudo: prefix.into_iter().map(Into::into).collect(),
        }
    }

    // ── Destructive (single-flight) ─────────────────────────────────────

    /// Restart the unit `target` names.
    ///
    /// **This is the only place in the panel that restarts a systemd unit**,
    /// and it takes a [`RestartTarget`] — a type constructible only by
    /// resolving a key against the server-side table in [`crate::config`] — so
    /// there is no signature here a client-supplied unit name could be passed
    /// through. `docs/PANEL_IA.md` § "Preserving the no-arbitrary-unit
    /// property".
    ///
    /// Scope decides privilege:
    ///
    /// * [`UnitScope::User`] → `systemctl --user restart <unit>`. No
    ///   elevation, and it keeps working with the daemon down — that is what
    ///   makes System ▸ Services a recovery surface rather than a convenience.
    /// * [`UnitScope::System`] → `sudo -n systemctl restart <unit>`, matching
    ///   the per-unit NOPASSWD sudoers line the `htpc_common` ansible role
    ///   ships (`docs/PANEL.md` § Deployment prerequisite). The argv is
    ///   deliberately `systemctl restart <unit>` with no `--` separator and no
    ///   extra flags: sudoers matches on the exact command line, so anything
    ///   else would silently stop matching the rule. Safe because `<unit>`
    ///   came out of the validated table, not off the wire.
    ///
    /// With no sudoers rule, `sudo -n` exits non-zero immediately and this
    /// returns [`ExecError::NotPermitted`] — never `Ok`, never a silent no-op.
    pub async fn restart(&self, target: &RestartTarget) -> Result<String, ExecError> {
        let _guard = self.lock.lock().await;
        // `target.unit().as_str()` is written out at both call sites rather
        // than bound to a local: `tests::the_only_mutating_systemctl_argv_is_a_
        // restart_target` reads this file and requires every argv element of a
        // mutating `systemctl` to be either a string literal or exactly that
        // expression, so a future edit cannot quietly interpose a `&str`.
        match target.scope() {
            UnitScope::User => {
                run(
                    "systemctl",
                    &["--user", "restart", target.unit().as_str()],
                    SYSTEMCTL_TIMEOUT,
                )
                .await
            }
            UnitScope::System => run_prefixed(
                &self.sudo,
                &["-n", "systemctl", "restart", target.unit().as_str()],
                SYSTEMCTL_TIMEOUT,
            )
            .await
            .map_err(classify_sudo_failure),
        }
    }

    /// `systemctl --user restart <daemon-unit>`.
    pub async fn restart_daemon(&self) -> Result<String, ExecError> {
        self.restart(&builtin("daemon")).await
    }

    /// `systemctl --user restart <shell-unit>`.
    pub async fn restart_shell(&self) -> Result<String, ExecError> {
        self.restart(&builtin("shell")).await
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

    /// `systemctl [--user] show <unit>` for the properties System ▸ Services
    /// renders: load/active/sub state, enabled-state, active-since and the
    /// failure detail.
    ///
    /// **Read-only, and therefore unrestricted** — any unit, either scope
    /// (`docs/PANEL_IA.md` § Services: reading unit status is inert). The
    /// operator-typed name still arrives as a [`UnitName`], so the validator
    /// cannot be bypassed by adding a caller; `--` additionally ends option
    /// parsing, which is free here because no sudoers rule has to match this
    /// command line.
    ///
    /// `systemctl show` exits 0 for a unit that does not exist, answering
    /// `LoadState=not-found` — the parser distinguishes that from an error.
    pub async fn show_unit(&self, scope: UnitScope, unit: &UnitName) -> Result<String, ExecError> {
        let mut args: Vec<&str> = Vec::new();
        if scope.is_user() {
            args.push("--user");
        }
        args.push("show");
        args.push("--no-pager");
        args.push(SHOW_PROPERTIES);
        args.push("--");
        args.push(unit.as_str());
        run("systemctl", &args, READ_TIMEOUT).await
    }

    /// `systemctl --user is-active <unit>`, trimmed. A spawn failure or
    /// timeout degrades to `"unknown"` rather than propagating an error —
    /// this is a status probe, not a control action.
    pub async fn unit_active(&self, unit: &UnitName) -> String {
        match run(
            "systemctl",
            &["--user", "is-active", unit.as_str()],
            READ_TIMEOUT,
        )
        .await
        {
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

/// [`run`], with `prefix` supplying the program and any leading arguments.
///
/// Split out so [`Recovery::sudo`] can be an argv prefix while the caller
/// still passes its own arguments as one literal slice — which is what
/// `tests::the_only_mutating_systemctl_argv_is_a_restart_target` scans.
async fn run_prefixed(
    prefix: &[String],
    args: &[&str],
    timeout: Duration,
) -> Result<String, ExecError> {
    let Some((program, leading)) = prefix.split_first() else {
        // Unreachable via `new()`; a caller-facing error rather than a panic
        // so an empty seam can never read as a successful restart.
        return Err(ExecError::Spawn("empty elevation argv prefix".to_string()));
    };
    let mut argv: Vec<&str> = leading.iter().map(String::as_str).collect();
    argv.extend_from_slice(args);
    run(program, &argv, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unit_active_never_panics_and_returns_some_string() {
        let recovery = Recovery::new();
        let status = recovery
            .unit_active(&UnitName::parse("definitely-not-a-real-unit.service").unwrap())
            .await;
        assert!(
            !status.is_empty(),
            "unit_active must always return a non-empty string"
        );
    }

    /// Build a one-entry restart table the way config load does — the only
    /// way to get a [`RestartTarget`], in tests as in production.
    fn target(key: &str, unit: &str, scope: &str) -> RestartTarget {
        let raw = [crate::config::RawManagedUnit {
            key: key.to_string(),
            unit: unit.to_string(),
            scope: scope.to_string(),
        }];
        crate::config::resolve_managed_units(&raw)
            .expect("well-formed test entry")
            .remove(0)
    }

    /// A stand-in for `sudo` that records the argv it was called with and
    /// refuses exactly the way `sudo -n` does with no matching sudoers rule.
    ///
    /// Owns its temp directory and removes it on drop — one leaked directory
    /// per invocation is what this used to do (tv-shell#428).
    struct FakeSudo {
        dir: std::path::PathBuf,
        script: std::path::PathBuf,
        marker: std::path::PathBuf,
    }

    impl FakeSudo {
        /// The argv prefix that runs this fake: `sh <script>`.
        ///
        /// **The script path is deliberately not exposed on its own.** Handing
        /// out only the `sh`-prefixed form is what keeps a future test from
        /// reintroducing the direct-exec `ETXTBSY` race — there is no accessor
        /// that would let it. See [`Recovery::with_sudo`].
        fn argv(&self) -> [String; 2] {
            ["sh".to_string(), self.script.to_string_lossy().into_owned()]
        }

        /// The argv the fake was last invoked with, or `""` if never invoked.
        fn recorded(&self) -> String {
            std::fs::read_to_string(&self.marker).unwrap_or_default()
        }

        /// Whether the fake was invoked at all.
        fn was_invoked(&self) -> bool {
            self.marker.exists()
        }
    }

    impl Drop for FakeSudo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fake_sudo(name: &str, refuse: bool) -> FakeSudo {
        let dir = std::env::temp_dir().join(format!(
            "tv-shell-panel-sudo-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("argv");
        let script = dir.join("fake-sudo");
        let body = if refuse {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {m}\n\
                 echo 'sudo: a password is required' >&2\nexit 1\n",
                m = marker.display()
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {m}\necho ok\n",
                m = marker.display()
            )
        };
        std::fs::write(&script, body).unwrap();
        // Still marked executable: `sh <script>` does not require it, but a
        // mode-0644 fake would silently pass for the wrong reason if the
        // prefix ever regressed to a direct exec.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        FakeSudo {
            dir,
            script,
            marker,
        }
    }

    /// **The behaviour htpc-1 exercises today**: a unit is in `managed_units`,
    /// no sudoers line exists for it, and `sudo -n` refuses. That must surface
    /// as [`ExecError::NotPermitted`] — not `Ok`, not a generic failure.
    #[tokio::test]
    async fn a_system_scope_restart_with_no_sudoers_rule_fails_closed() {
        let fake = fake_sudo("refuse", true);
        let recovery = Recovery::with_sudo(fake.argv());
        let err = recovery
            .restart(&target("sshd", "sshd.service", "system"))
            .await
            .expect_err("a refused sudo must never read as success");
        match &err {
            ExecError::NotPermitted(detail) => {
                assert!(
                    detail.contains("a password is required"),
                    "sudo's own line must survive into the error: {detail}"
                );
            }
            other => panic!("expected NotPermitted, got {other:?}"),
        }
        // And it really did try, with the exact argv the sudoers rule matches.
        assert_eq!(
            fake.recorded().trim(),
            "-n systemctl restart sshd.service",
            "the argv must stay exactly what the sudoers rule matches"
        );
    }

    /// `systemctl --user` needs no elevation, and routing it through `sudo`
    /// would break the one thing that keeps working with the daemon down.
    #[tokio::test]
    async fn a_user_scope_restart_never_touches_sudo() {
        let fake = fake_sudo("unused", false);
        let recovery = Recovery::with_sudo(fake.argv());
        // A unit that does not exist: the point is which binary is spawned,
        // and `systemctl --user restart` on a missing unit is inert.
        let _ = recovery
            .restart(&target(
                "nope",
                "tv-shell-panel-no-such-test-unit.service",
                "user",
            ))
            .await;
        assert!(
            !fake.was_invoked(),
            "a user-scope restart must not invoke the elevation helper"
        );

        // The seam is only safe because production never uses it. Widening
        // `sudo` from a program name to an argv prefix widened what a caller
        // *could* put there, so pin the production value rather than trusting
        // that `with_sudo` stays `#[cfg(test)]`.
    }

    /// The elevation prefix a real node runs with is exactly `sudo` — no
    /// interpreter, no extra flags, nothing the sudoers rule would stop
    /// matching.
    #[test]
    fn the_production_elevation_prefix_is_exactly_sudo() {
        assert_eq!(
            Recovery::new().sudo,
            vec!["sudo".to_string()],
            "production must elevate through bare `sudo`; the argv prefix is a \
             test seam and must never carry anything else"
        );
    }

    #[test]
    fn sudo_refusals_are_told_apart_from_the_wrapped_command_failing() {
        for refusal in [
            "sudo: a password is required",
            "sudo: no tty present and no askpass program specified",
            "Sorry, user tv-shell is not allowed to execute '/usr/bin/systemctl restart x' as root.",
        ] {
            assert!(is_sudo_refusal(refusal), "{refusal:?} is a refusal");
            assert!(matches!(
                classify_sudo_failure(ExecError::NonZero(1, refusal.to_string())),
                ExecError::NotPermitted(_)
            ));
        }
        // The unit itself failing to restart is NOT a permission problem, and
        // must not be reported as one.
        let real =
            "Job for sshd.service failed because the control process exited with error code.";
        assert!(!is_sudo_refusal(real));
        assert!(matches!(
            classify_sudo_failure(ExecError::NonZero(1, real.to_string())),
            ExecError::NonZero(1, _)
        ));
        // No sudo at all is, operationally, the same as no rule.
        assert!(matches!(
            classify_sudo_failure(ExecError::Spawn("No such file or directory".to_string())),
            ExecError::NotPermitted(_)
        ));
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
