//! Scope launching — the Rust port of `dev/gamescope/lib.sh`'s
//! `gs_scope_ready` / `gs_scope_unit` / `gs_scope_run` / `gs_scope_of`.
//!
//! # Why the cgroup scope is the primary id
//!
//! gamescope's only cgroup parser is `sscanf("app-steam-app%u-%d.scope")` in
//! `src/Utils/Process.cpp`, evaluated at window creation from the pid it gets via
//! `XResQueryClientIds`. So a process launched into
//! `app-steam-app<appid>-<pid>.scope` has every window it (or any child — a
//! process family inherits the cgroup) creates resolved to `<appid>` with no
//! tagging at all. That name is an **upstream contract**, not ours to rename: a
//! prefix we invented would be a name the compositor never reads, which is the
//! mirror image of v1's silent-success class and is exactly what §5 warns about.
//!
//! This was measured, not assumed. From the kit's own record: a plain launch with
//! post-hoc tagging attempted left `GAMESCOPE_FOCUSABLE_WINDOWS` **empty**, while
//! the same command inside `app-steam-app9003-2970.scope` produced
//! `8388625, 9003, 2998` and `focus=9003` with `STEAM_GAME` never set.
//!
//! Hence §5's rule: **scope first, tag as repair, never by name.** `STEAM_GAME`
//! is written only where the scope did not resolve (a pid namespace — Plex under
//! `bwrap` — or a browser that handed off to an already-running instance).
//!
//! # Properties carried over from the shell version
//!
//! * **Preflight is fail-closed with no unscoped fallback.** An unscoped launch
//!   is the broken case above: it would appear to succeed and fail silently at
//!   the far end, with the app unreachable. [`ScopeEnv::detect`] refuses instead.
//! * **`DBUS_SESSION_BUS_ADDRESS` is derived from `XDG_RUNTIME_DIR` only when
//!   `<runtime_dir>/bus` is really a SOCKET** (`[ -S ... ]` in the shell
//!   version, a file-type check here). Exporting a bus address that points at
//!   nothing — or at a leftover regular file — turns a clear "no session bus"
//!   error into a confusing D-Bus timeout.
//! * **The unit is named after the pid that becomes the app.** The shell version
//!   uses `$BASHPID`, not `$$`, because `$(...)` forks a subshell whose `$$` is
//!   the parent's pid; naming the scope after the wrong pid would make two
//!   concurrent launches collide. Rust has no such trap — the unit name is
//!   formatted from the child's own pid — but the *reason* the name must be
//!   unique per launch stands: a supervisor relaunching its child must never
//!   collide with a scope whose processes have not been reaped.
//! * **`--collect`** so a scope that ended up failed is removed; nothing else
//!   would remove it.
//! * **No `--quiet`.** `man systemd-run` says it "may not be combined with
//!   ... --scope". Current systemd tolerates the pair, but relying on
//!   undocumented tolerance is precisely the mistake the kit made with post-hoc
//!   tagging, and a point release ended that. The `Running as unit: <name>` line
//!   it would suppress is useful anyway — it records the scope name in the
//!   client's own log, independently of us.

use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::atoms::AppId;

/// The `systemd-run` binary. Resolved through `PATH` like the shell version.
const SYSTEMD_RUN: &str = "systemd-run";

/// Everything that can stop a scoped launch, as typed variants.
///
/// Each carries the operator-facing sentence the shell version printed to
/// stderr, because "systemd-run failed" with no reason is what made these
/// failures expensive to diagnose in the first place.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error(
        "no `systemd-run` on PATH; gamescope identifies an app by its cgroup scope, \
         so a scope-less launch cannot be focused"
    )]
    NoSystemdRun,
    #[error(
        "XDG_RUNTIME_DIR is unset, so `systemd-run --user` has no session bus to talk to; \
         source the session environment file first, which carries both it and \
         DBUS_SESSION_BUS_ADDRESS"
    )]
    NoRuntimeDir,
    #[error(
        "DBUS_SESSION_BUS_ADDRESS is unset and {0}/bus is not a socket; \
         `systemd-run --user` has no session bus"
    )]
    NoSessionBus(String),
    #[error("no command to launch")]
    EmptyCommand,
    #[error("spawning `systemd-run --user --scope --unit={unit}`: {source}")]
    Spawn {
        unit: String,
        #[source]
        source: std::io::Error,
    },
    /// The launcher exited before it could be confirmed. A misspelled binary, a
    /// `--unit=` collision, a bus that went away after the preflight — all of
    /// them land here, and all of them used to reply with a JSON success payload
    /// naming a pid that was already dead.
    #[error(
        "`systemd-run --user --scope --unit={unit}` exited immediately ({status}); \
         the app was NOT launched. Check the journal for systemd-run's own error line"
    )]
    ExitedImmediately { unit: String, status: String },
    /// The process is alive but is not in the scope we asked for.
    ///
    /// This is the failure that matters most, because it is the one that used to
    /// be invisible: gamescope identifies an app **only** by its cgroup scope, so
    /// a live process outside one is a window no focus rule can ever name.
    #[error(
        "launched pid {pid} is not in {unit}.scope after {waited_ms} ms \
         (bound {bound_ms} ms){detail}; gamescope resolves an app id from the cgroup \
         scope alone, so this window would be unfocusable"
    )]
    NotScoped {
        unit: String,
        pid: u32,
        waited_ms: u64,
        bound_ms: u64,
        /// What the pid's cgroup said instead, when it said anything.
        detail: String,
    },
    #[error("reading the launched process's state: {0}")]
    Confirm(#[source] std::io::Error),
}

/// A verified session environment for `systemd-run --user`.
///
/// Constructed only by [`ScopeEnv::detect`] / [`ScopeEnv::resolve`], so holding
/// one is proof the preflight passed — a launch cannot skip it by construction.
/// The fields are **private for exactly that reason**: a `pub` field would let a
/// caller assemble one by literal and bypass the preflight, which is the
/// unscoped launch this module exists to make unreachable. Read them through
/// [`Self::runtime_dir`] / [`Self::dbus_address`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEnv {
    /// `XDG_RUNTIME_DIR`.
    runtime_dir: String,
    /// The session bus address to hand the child, whether inherited or derived.
    dbus_address: String,
}

impl ScopeEnv {
    /// Run the preflight against the process environment.
    pub fn detect() -> Result<Self, LaunchError> {
        Self::resolve(
            which(SYSTEMD_RUN),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
            // The shell version tests `[ -S ... ]`, and so does this. A plain
            // existence check would accept a leftover regular file or a
            // directory at `<runtime_dir>/bus` and then hand the child a bus
            // address pointing at something that cannot be dialled — a confusing
            // D-Bus timeout in place of the clear "no session bus" the error
            // text promises.
            |p| {
                std::fs::metadata(Path::new(p))
                    .map(|m| m.file_type().is_socket())
                    .unwrap_or(false)
            },
        )
    }

    /// `XDG_RUNTIME_DIR`, as verified by the preflight.
    pub fn runtime_dir(&self) -> &str {
        &self.runtime_dir
    }

    /// The session bus address handed to launched children.
    pub fn dbus_address(&self) -> &str {
        &self.dbus_address
    }

    /// The pure half of the preflight — no environment, no filesystem.
    ///
    /// `bus_is_socket` answers "is `<runtime_dir>/bus` a SOCKET?"; in production
    /// that is a stat plus a file-type check, in tests it is a closure, which is
    /// what makes every branch of the fail-closed policy testable without a
    /// session bus.
    pub fn resolve(
        has_systemd_run: bool,
        runtime_dir: Option<&str>,
        dbus_address: Option<&str>,
        bus_is_socket: impl Fn(&str) -> bool,
    ) -> Result<Self, LaunchError> {
        if !has_systemd_run {
            return Err(LaunchError::NoSystemdRun);
        }
        let runtime_dir = match runtime_dir {
            Some(d) if !d.is_empty() => d.to_string(),
            _ => return Err(LaunchError::NoRuntimeDir),
        };
        // An inherited address wins; we only derive when there is none, and only
        // when the socket is really there. Deliberately no fallback beyond that.
        let dbus_address = match dbus_address {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                let candidate = format!("{runtime_dir}/bus");
                if bus_is_socket(&candidate) {
                    format!("unix:path={candidate}")
                } else {
                    return Err(LaunchError::NoSessionBus(runtime_dir));
                }
            }
        };
        Ok(Self {
            runtime_dir,
            dbus_address,
        })
    }
}

/// Format the transient unit name for a launch.
///
/// Returned **without** the `.scope` suffix, which is the form
/// `systemd-run --unit` takes; [`scope_of`] matches the suffixed form as it
/// appears in `/proc/<pid>/cgroup`.
pub fn scope_unit(app_id: AppId, launcher_pid: u32) -> String {
    format!("app-steam-app{}-{}", app_id.get(), launcher_pid)
}

/// The full argv for a scoped launch, as a testable value.
///
/// Separated from the spawn so the exact flag order — the part gamescope's
/// parser and systemd's own contract depend on — is asserted in a unit test
/// rather than only exercised on hardware.
pub fn scope_argv(unit: &str, command: &[String]) -> Vec<String> {
    let mut argv = vec![
        SYSTEMD_RUN.to_string(),
        "--user".to_string(),
        "--scope".to_string(),
        "--collect".to_string(),
        format!("--unit={unit}"),
        "--".to_string(),
    ];
    argv.extend_from_slice(command);
    argv
}

/// The per-app-class environment applied to a launched process.
///
/// Two lists, because the operations are genuinely different and only one of
/// them can be expressed as a value: `set` writes variables, `unset` REMOVES
/// them. See [`crate::config::AppConfig`] for the measurement that made removal
/// load-bearing (Moonlight goes native Wayland and never maps a window unless
/// `WAYLAND_DISPLAY` is *absent* — an empty string is not the same thing, and
/// pressure-vessel rewrites an empty one back to `wayland-0`).
///
/// Borrowed rather than owned so the caller's config is the single copy.
#[derive(Debug, Default, Clone, Copy)]
pub struct LaunchEnv<'a> {
    /// Variables to set, as `(name, value)`.
    pub set: &'a [(String, String)],
    /// Variables to remove. Applied AFTER `set`, so a name in both is removed.
    pub unset: &'a [String],
}

/// Build the `Command` for a scoped launch, with the class environment applied.
///
/// Split out from [`launch`] for the same reason [`scope_argv`] is: it is the
/// part whose exact shape has to be asserted, and a `Command` can be inspected
/// (`get_envs`) without being run. So the test reads the REAL command this
/// function hands to `spawn`, not a re-derivation of what it ought to contain —
/// which matters here, because "the env table is applied" and "the env table is
/// computed correctly" are different claims and only the first one is the bug
/// that reaches the television.
///
/// The scope environment is applied first and the class environment second, so a
/// class cannot accidentally break `systemd-run --user` by overwriting
/// `XDG_RUNTIME_DIR`... except deliberately, which is left possible on purpose:
/// an operator who sets it in a class has said something specific, and silently
/// ignoring a key an operator wrote is the failure mode this crate keeps
/// removing. `unset` last, per its documented precedence.
pub fn prepare_command(argv: &[String], scope: &ScopeEnv, env: LaunchEnv<'_>) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env("XDG_RUNTIME_DIR", &scope.runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", &scope.dbus_address)
        .stdin(Stdio::null());
    for (name, value) in env.set {
        cmd.env(name, value);
    }
    for name in env.unset {
        cmd.env_remove(name);
    }
    cmd
}

/// A launched app: the pid that owns the scope, and the scope's name.
///
/// **Only constructed for a CONFIRMED launch** — see [`launch`]. Every field is
/// a fact read back after the spawn, not a hope recorded before it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Launched {
    pub app_id: AppId,
    pub pid: u32,
    /// The unit name **with** its `.scope` suffix, as it appears in cgroup paths.
    pub scope: String,
    /// How long the confirmation took. A number creeping toward the bound is the
    /// early warning that this path is degrading, the same reason
    /// [`crate::baselayer::Switched::took_ms`] is returned rather than dropped.
    pub confirmed_ms: u64,
}

/// Launch `command` inside a transient `app-steam-app<appid>-<pid>.scope`,
/// **and confirm it before reporting success**.
///
/// # Why a spawn is not a launch
///
/// `Command::spawn()` returns `Ok` the instant `systemd-run` is forked. It says
/// nothing about whether the binary exists, whether the session bus was still
/// there, or whether the `--unit=` name collided with a scope whose processes
/// had not been reaped. This function used to return its `Launched` payload
/// straight off that `Ok`, with the exit status consumed only by a detached
/// reaper thread and logged at `debug!` — so every one of those failures replied
/// with a JSON **success** naming a dead pid and a scope that never existed.
///
/// That is v1's silent-success class (§3) reproduced exactly, in the module
/// whose doc comment says the unscoped case "would appear to succeed and fail
/// silently at the far end". So the launch confirms itself:
///
/// 1. The launcher has not already exited (`try_wait`).
/// 2. `/proc/<pid>/cgroup` names the scope we asked for ([`scope_of`]) — which
///    is the one honest post-launch confirmation available without asking the
///    compositor, because it is *exactly the string gamescope parses*.
///
/// Both are polled to a bound rather than checked once: the scope shows up a few
/// milliseconds after the spawn. A launch that cannot be confirmed inside the
/// bound is an `Err`, never a `Launched`.
///
/// # What it still does not wait for
///
/// A confirmed launch means the process is alive in the right cgroup. It does
/// **not** mean a window has mapped — that wait belongs to
/// [`crate::baselayer`]'s map bound, and conflating the two is what made a
/// `show` after a `launch` fail on every working launch.
///
/// # Reaping
///
/// std does not reap a `Child` on drop and the core installs no `SIGCHLD`
/// handler, so a spawned-and-forgotten launch stays a zombie for the core's
/// whole lifetime — holding a pid the kernel cannot reuse and, worse for this
/// module, keeping [`scope_of`] answering confidently about a process that is
/// dead. So the `Child` is moved into a detached thread whose only job is to
/// `wait()` it and log the exit status. **That happens on every path, including
/// the failure paths**, or a refused launch would leak the very zombie the
/// reaper exists to prevent.
///
/// Note the pid recorded is `systemd-run`'s own. That is correct and load-
/// bearing: `systemd-run --scope` **execs** the target command, so the pid the
/// caller holds is the app's own pid the whole way down (verified by the kit),
/// which keeps pid-matching, family walks and `wait` working unchanged.
pub fn launch(
    env: &ScopeEnv,
    app_id: AppId,
    command: &[String],
    launch_env: LaunchEnv<'_>,
    confirm_timeout: Duration,
) -> Result<Launched, LaunchError> {
    if command.is_empty() {
        return Err(LaunchError::EmptyCommand);
    }
    // The `%d` field: the shell version uses `$BASHPID`, the pid of the subshell
    // that then execs into the app. Rust cannot do that without forking by hand
    // — the unit name is an argv element, so it must be chosen BEFORE the spawn
    // that would reveal the child's pid. gamescope parses this field as an
    // opaque `%d` and never resolves it to a process (`src/Utils/Process.cpp`
    // keeps only the `%u`), so uniqueness per launch is the entire requirement,
    // and [`next_launch_tag`] supplies it. Nothing downstream reads the tag as a
    // pid; the pid the caller needs is returned in [`Launched::pid`].
    let launcher_pid = next_launch_tag();
    let unit = scope_unit(app_id, launcher_pid);
    let argv = scope_argv(&unit, command);

    // NOTE the env goes on `systemd-run` itself, which is correct: `--scope`
    // EXECS the target command in the same process, so the child's environment
    // is this one. (`systemd-run --user --scope` does not sanitise it the way a
    // service unit's `Environment=` would — there is no manager in between.)
    let mut child = prepare_command(&argv, env, launch_env)
        .spawn()
        .map_err(|source| LaunchError::Spawn {
            unit: unit.clone(),
            source,
        })?;

    // The real pid, read BEFORE the child moves into the reaper.
    let pid = child.id();
    let outcome = confirm(
        &unit,
        pid,
        confirm_timeout,
        || child.try_wait(),
        || scope_of(pid),
        || std::thread::sleep(CONFIRM_POLL_INTERVAL),
    );

    // Reap on EVERY path. A refused launch that leaks a zombie is worse than the
    // silent success it replaced.
    let reaped = format!("{unit}.scope");
    std::thread::spawn(move || match child.wait() {
        Ok(status) => {
            tracing::debug!(pid, scope = %reaped, %status, "launched process reaped")
        }
        Err(e) => {
            tracing::debug!(pid, scope = %reaped, error = %e, "waiting on launched process failed")
        }
    });

    finish(app_id, pid, &unit, outcome)
}

/// Turn a confirmation outcome into the reply.
///
/// One line, and split out anyway, because it is THE line: it is where an
/// unconfirmed launch either becomes an `Err` or gets laundered into a
/// `Launched` naming a dead pid, which is what this code used to do. Everything
/// above it is a real spawn, so without this seam the rule would be defended
/// only by tests of `confirm` — which say nothing about whether anyone acts on
/// its answer.
fn finish(
    app_id: AppId,
    pid: u32,
    unit: &str,
    confirmed: Result<u64, LaunchError>,
) -> Result<Launched, LaunchError> {
    Ok(Launched {
        app_id,
        pid,
        scope: format!("{unit}.scope"),
        confirmed_ms: confirmed?,
    })
}

/// How often the confirmation loop re-reads `/proc` while waiting.
const CONFIRM_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The confirmation itself, with every effect injected.
///
/// `exited` is `Child::try_wait`, `probe` is [`scope_of`], `wait` is the sleep.
/// Injected so the whole decision — including "the launcher died", which cannot
/// be provoked reliably by spawning a real process — is unit-testable with no
/// systemd, no D-Bus and no `/proc` at all.
///
/// Returns how many milliseconds the confirmation took.
#[allow(clippy::type_complexity)]
fn confirm(
    unit: &str,
    pid: u32,
    timeout: Duration,
    mut exited: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
    mut probe: impl FnMut() -> Option<Scope>,
    mut wait: impl FnMut(),
) -> Result<u64, LaunchError> {
    let started = Instant::now();
    let want = format!("{unit}.scope");
    let mut last_seen: Option<String> = None;
    loop {
        // Ordered deliberately: an exit is checked FIRST, because a launcher
        // that has already died is a definite answer and the cgroup read for a
        // dead pid is just an absent file that looks like "not yet".
        if let Some(status) = exited().map_err(LaunchError::Confirm)? {
            return Err(LaunchError::ExitedImmediately {
                unit: unit.to_string(),
                status: status.to_string(),
            });
        }
        match probe() {
            Some(scope) if scope.unit == want => return Ok(started.elapsed().as_millis() as u64),
            // In an `app-steam-app*` scope, but not OURS. Two launches racing a
            // unit name, or a pid reused. Worth naming: it is the difference
            // between "no scope" and "someone else's app id".
            Some(other) => last_seen = Some(other.unit),
            None => {}
        }
        if started.elapsed() >= timeout {
            return Err(LaunchError::NotScoped {
                unit: unit.to_string(),
                pid,
                waited_ms: started.elapsed().as_millis() as u64,
                bound_ms: timeout.as_millis() as u64,
                detail: match last_seen {
                    Some(u) => format!(", it is in {u} instead"),
                    None => String::new(),
                },
            });
        }
        wait();
    }
}

/// A per-process monotonic tag for the scope name's second field.
///
/// gamescope reads it as an opaque `%d` and never resolves it to a process, so
/// its only job is to make the unit name unique per launch — the property the
/// shell version got from `$BASHPID`.
///
/// **The guarantee is exactly "unique within this process".** Seeding from our
/// own pid makes a collision between two cores (a restart mid-flight, a dev
/// copy) unlikely, not impossible: `pid * 1000 + seq` collides for pids exactly
/// 1,000,000 apart, which is inside Linux's default `pid_max`. Widening the tag
/// would buy fake precision, because the property that actually matters is
/// already enforced downstream — systemd refuses a `--unit=` name that is
/// already live, so a cross-process collision fails LOUDLY at `systemd-run`
/// rather than quietly sharing a scope with someone else's app.
fn next_launch_tag() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // Keep it inside %d's range and readable in a unit name.
    (std::process::id().wrapping_mul(1000).wrapping_add(seq)) % 1_000_000_000
}

/// Parse the app id back out of a `/proc/<pid>/cgroup` body.
///
/// The grammar is exactly gamescope's `app-steam-app%u-%d.scope`, matched at the
/// END of a cgroup path segment, mirroring the shell version's
/// `sed -n 's,.*/\(app-steam-app[0-9]*-[0-9]*\.scope\)$,\1,p'`.
///
/// Returns `None` for a process in no such scope — an unscoped launch, or a
/// kernel too old for cgroup v2. That is informational and not an error: it is
/// what gamescope reads, so it is the one honest post-launch confirmation
/// available without asking the compositor.
pub fn parse_cgroup_scope(cgroup: &str) -> Option<Scope> {
    cgroup.lines().find_map(|line| {
        let segment = line.rsplit('/').next()?;
        parse_scope_name(segment)
    })
}

/// A resolved `app-steam-app<appid>-<tag>.scope`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Scope {
    pub app_id: AppId,
    /// The `%d` field. Opaque to gamescope; uniqueness is its whole job.
    pub tag: u32,
    /// The full unit name including `.scope`.
    pub unit: String,
}

/// Match one cgroup path segment against the upstream grammar.
fn parse_scope_name(segment: &str) -> Option<Scope> {
    let body = segment
        .strip_prefix("app-steam-app")?
        .strip_suffix(".scope")?;
    let (app, tag) = body.split_once('-')?;
    // `%u` and `%d` are non-empty runs of digits. A leading `+`/`-` or any other
    // character is not this grammar, and guessing past it is how a shape
    // mismatch becomes an invisible wrong answer.
    if app.is_empty() || tag.is_empty() {
        return None;
    }
    if !app.bytes().all(|b| b.is_ascii_digit()) || !tag.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Scope {
        app_id: AppId::new(app.parse().ok()?),
        tag: tag.parse().ok()?,
        unit: segment.to_string(),
    })
}

/// The scope a live pid is in, read from `/proc/<pid>/cgroup`.
///
/// `None` when the process is in no `app-steam-app*` scope, or is gone.
pub fn scope_of(pid: u32) -> Option<Scope> {
    let body = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    parse_cgroup_scope(&body)
}

/// Is `path` a regular file with an execute bit set?
///
/// `is_file()` alone accepted a non-executable file of the right name, so the
/// preflight would pass and the spawn would then fail with `EACCES` — turning a
/// clear "no `systemd-run` on PATH" into an obscure one. `which(1)` checks the
/// x-bit; so does this.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Is `name` on `PATH`?
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(name))))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    // -- the per-app-class launch environment --------------------------------
    //
    // MEASURED ON HARDWARE 2026-09-06: a bare `/usr/bin/moonlight` inside the v2
    // session inherits `WAYLAND_DISPLAY=gamescope-0`, selects native Wayland, and
    // never maps a window — so the base layer is set correctly and the screen
    // stays black. The working invocation removes that variable and sets three
    // others. These tests are about the REMOVAL, because a set-only environment
    // could not express it and no value substitutes for absence.

    /// A `ScopeEnv` without touching the process environment.
    fn test_scope_env() -> ScopeEnv {
        ScopeEnv::resolve(
            true,
            Some("/run/user/1000"),
            Some("unix:path=/run/user/1000/bus"),
            |_| true,
        )
        .expect("a fully-specified preflight resolves")
    }

    /// The Moonlight class as it ships in `config/core.toml.example`.
    fn moonlight_env() -> (Vec<(String, String)>, Vec<String>) {
        (
            vec![
                ("QT_QPA_PLATFORM".to_string(), "xcb".to_string()),
                ("SDL_VIDEODRIVER".to_string(), "x11".to_string()),
                ("ENABLE_GAMESCOPE_WSI".to_string(), "1".to_string()),
            ],
            vec!["WAYLAND_DISPLAY".to_string()],
        )
    }

    /// The env operations reach the REAL `Command` the launch spawns.
    ///
    /// Asserted by inspecting `get_envs()` on the command `prepare_command`
    /// returns, not by re-deriving what it ought to contain — "the table is
    /// computed correctly" and "the table is applied" are different claims, and
    /// only the second one is the bug that reaches the television.
    ///
    /// Mutation-check: delete the `env_remove` loop from `prepare_command` and
    /// the `WAYLAND_DISPLAY => None` assertion below fails.
    #[test]
    fn the_class_environment_reaches_the_spawned_command() {
        let scope = test_scope_env();
        let (set, unset) = moonlight_env();
        let argv = vec!["systemd-run".to_string(), "--scope".to_string()];
        let cmd = prepare_command(
            &argv,
            &scope,
            LaunchEnv {
                set: &set,
                unset: &unset,
            },
        );

        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let get = |name: &str| {
            envs.iter()
                .find(|(k, _)| k == name)
                .unwrap_or_else(|| panic!("{name} is not among the command's env ops: {envs:?}"))
                .clone()
        };

        // The three sets.
        assert_eq!(get("QT_QPA_PLATFORM").1.as_deref(), Some("xcb"));
        assert_eq!(get("SDL_VIDEODRIVER").1.as_deref(), Some("x11"));
        assert_eq!(get("ENABLE_GAMESCOPE_WSI").1.as_deref(), Some("1"));

        // THE UNSET. `None` is how std records a removal; a variable that were
        // merely set to "" would show as `Some("")` here — and pressure-vessel
        // rewrites an empty WAYLAND_DISPLAY back to `wayland-0` (§11), so the
        // difference between these two is the whole bug.
        assert_eq!(
            get("WAYLAND_DISPLAY").1,
            None,
            "WAYLAND_DISPLAY must be REMOVED, not set to anything"
        );

        // And the scope environment is still there — a class must not cost the
        // launch its session bus.
        assert_eq!(get("XDG_RUNTIME_DIR").1.as_deref(), Some("/run/user/1000"));
        assert_eq!(
            get("DBUS_SESSION_BUS_ADDRESS").1.as_deref(),
            Some("unix:path=/run/user/1000/bus")
        );
    }

    /// End-to-end: the variable is genuinely absent from a REAL child process.
    ///
    /// The test above asserts the recorded intent; this one runs `/usr/bin/env`
    /// through the same `prepare_command` with `WAYLAND_DISPLAY` set in this
    /// process, and reads the child's actual environment back. That is the claim
    /// the hardware failure was about — "removed" has to mean the child cannot
    /// see it, not that we asked nicely.
    ///
    /// Mutation-check: delete the `env_remove` loop and this fails too, on the
    /// child's own output.
    #[test]
    fn wayland_display_is_absent_from_the_real_child_environment() {
        // Serialized and restored: `set_var` is process-global, and this test
        // spawns children — the same discipline `config.rs` uses for its own env
        // mutation. Note `WAYLAND_DISPLAY` is read by nothing else in this crate.
        static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());

        let env_bin = ["/usr/bin/env", "/bin/env"]
            .into_iter()
            .find(|p| Path::new(p).exists());
        let Some(env_bin) = env_bin else {
            // Not a silent skip: say so, loudly, in the one environment that
            // could lack coreutils. The assertion above still covers the wiring.
            panic!("no env(1) binary found; this test needs coreutils");
        };

        let prev = std::env::var_os("WAYLAND_DISPLAY");
        // SAFETY: serialized by ENV_GUARD; restored before returning.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", "gamescope-0") };

        let (set, unset) = moonlight_env();
        let out = prepare_command(
            &[env_bin.to_string()],
            &test_scope_env(),
            LaunchEnv {
                set: &set,
                unset: &unset,
            },
        )
        .stdin(Stdio::null())
        .output();

        // SAFETY: serialized by ENV_GUARD; this is the restore half, and it runs
        // before any assertion so a failure cannot leak the variable.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
                None => std::env::remove_var("WAYLAND_DISPLAY"),
            }
        }

        let out = out.expect("running env(1)");
        assert!(out.status.success(), "env(1) failed: {out:?}");
        let child_env = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = child_env
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();

        assert!(
            !names.contains(&"WAYLAND_DISPLAY"),
            "the child can still see WAYLAND_DISPLAY, so Moonlight would select \
             native Wayland and never map a window. Child env: {child_env}"
        );
        assert!(
            child_env.contains("QT_QPA_PLATFORM=xcb"),
            "the child is missing QT_QPA_PLATFORM=xcb: {child_env}"
        );
    }

    /// `unset` wins over `set` for the same name, as the type documents.
    ///
    /// Not hypothetical: an operator who lists a variable in both has expressed
    /// a contradiction, and the resolution has to be the one that is safe —
    /// absence, which is what the app class needed in the first place.
    #[test]
    fn a_name_in_both_lists_is_removed() {
        let set = vec![("WAYLAND_DISPLAY".to_string(), "gamescope-0".to_string())];
        let unset = vec!["WAYLAND_DISPLAY".to_string()];
        let cmd = prepare_command(
            &["true".to_string()],
            &test_scope_env(),
            LaunchEnv {
                set: &set,
                unset: &unset,
            },
        );
        let recorded = cmd
            .get_envs()
            .find(|(k, _)| k.to_string_lossy() == "WAYLAND_DISPLAY")
            .expect("the name is recorded")
            .1;
        assert!(recorded.is_none(), "unset must win over set for one name");
    }

    /// An empty class environment leaves the launch exactly as it was before
    /// this feature — no accidental behaviour change for a class that declares
    /// no env, and for the ad-hoc `launch <id> <cmd>` form on an unknown id.
    #[test]
    fn an_empty_class_environment_changes_nothing() {
        let cmd = prepare_command(
            &["true".to_string()],
            &test_scope_env(),
            LaunchEnv::default(),
        );
        // `get_envs` yields the recorded ops in a sorted order, so compare as a
        // set rather than pinning an order std owns and we do not.
        let mut names: Vec<String> = cmd
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "DBUS_SESSION_BUS_ADDRESS".to_string(),
                "XDG_RUNTIME_DIR".to_string()
            ],
            "only the scope environment should be applied"
        );
    }
    use super::*;

    /// A real cgroup v2 body, in the shape `/proc/<pid>/cgroup` actually has on
    /// a systemd user session: one `0::` line, full unit path.
    const REAL_CGROUP: &str =
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steam-app9003-2970.scope\n";

    #[test]
    fn unit_name_matches_the_upstream_grammar() {
        assert_eq!(scope_unit(AppId::new(9003), 2970), "app-steam-app9003-2970");
        assert_eq!(scope_unit(AppId::new(0), 1), "app-steam-app0-1");
    }

    #[test]
    fn unit_name_round_trips_through_the_parser() {
        let unit = scope_unit(AppId::new(413091), 12345);
        let scope = parse_scope_name(&format!("{unit}.scope")).unwrap();
        assert_eq!(scope.app_id, AppId::new(413091));
        assert_eq!(scope.tag, 12345);
    }

    #[test]
    fn parses_a_real_cgroup_body() {
        let scope = parse_cgroup_scope(REAL_CGROUP).unwrap();
        assert_eq!(scope.app_id, AppId::new(9003));
        assert_eq!(scope.tag, 2970);
        assert_eq!(scope.unit, "app-steam-app9003-2970.scope");
    }

    #[test]
    fn an_unscoped_process_is_none_not_an_error() {
        let body = "0::/user.slice/user-1000.slice/session-3.scope\n";
        assert_eq!(parse_cgroup_scope(body), None);
    }

    #[test]
    fn a_different_app_scope_is_not_ours() {
        let body = "0::/user.slice/app.slice/app-firefox-1234.scope\n";
        assert_eq!(parse_cgroup_scope(body), None);
    }

    #[test]
    fn malformed_scope_names_are_rejected_not_guessed() {
        for bad in [
            "app-steam-app.scope",           // no fields
            "app-steam-app9003.scope",       // no tag
            "app-steam-app-2970.scope",      // no appid
            "app-steam-app9003-.scope",      // empty tag
            "app-steam-appx-2970.scope",     // non-numeric appid
            "app-steam-app9003-2x70.scope",  // non-numeric tag
            "app-steam-app9003-2970",        // missing suffix
            "app-steam-app9003-2970.slice",  // wrong unit type
            "xapp-steam-app9003-2970.scope", // wrong prefix
            "app-steam-app-1-2970.scope",    // signed appid
            "",
        ] {
            assert_eq!(parse_scope_name(bad), None, "should not parse: {bad:?}");
        }
    }

    #[test]
    fn overflowing_appid_is_rejected_rather_than_wrapped() {
        // u32::MAX + 1
        assert_eq!(parse_scope_name("app-steam-app4294967296-1.scope"), None);
    }

    #[test]
    fn empty_cgroup_body_is_none() {
        assert_eq!(parse_cgroup_scope(""), None);
        assert_eq!(parse_cgroup_scope("\n\n"), None);
    }

    #[test]
    fn cgroup_v1_multiline_body_still_finds_the_scope() {
        let body = "12:pids:/user.slice/app-steam-app770-99.scope\n\
                    0::/user.slice/app.slice/app-steam-app770-99.scope\n";
        assert_eq!(parse_cgroup_scope(body).unwrap().app_id, AppId::new(770));
    }

    #[test]
    fn argv_is_exactly_the_documented_flags_in_order() {
        let argv = scope_argv(
            "app-steam-app9003-2970",
            &["moonlight".to_string(), "stream".to_string()],
        );
        assert_eq!(
            argv,
            vec![
                "systemd-run",
                "--user",
                "--scope",
                "--collect",
                "--unit=app-steam-app9003-2970",
                "--",
                "moonlight",
                "stream",
            ]
        );
    }

    #[test]
    fn argv_has_no_quiet_flag() {
        // `man systemd-run`: --quiet "may not be combined with ... --scope".
        let argv = scope_argv("u", &["x".to_string()]);
        assert!(
            !argv.iter().any(|a| a == "--quiet" || a == "-q"),
            "{argv:?}"
        );
    }

    #[test]
    fn argv_terminates_options_before_the_command() {
        // Without `--`, a command whose first token starts with `-` would be
        // eaten by systemd-run as one of its own options.
        let argv = scope_argv("u", &["--weird-binary".to_string()]);
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[dashdash + 1], "--weird-binary");
    }

    #[test]
    fn preflight_refuses_without_systemd_run() {
        let err = ScopeEnv::resolve(false, Some("/run/user/1000"), None, |_| true).unwrap_err();
        assert!(matches!(err, LaunchError::NoSystemdRun), "{err}");
    }

    #[test]
    fn preflight_refuses_without_runtime_dir() {
        for dir in [None, Some("")] {
            let err = ScopeEnv::resolve(true, dir, None, |_| true).unwrap_err();
            assert!(matches!(err, LaunchError::NoRuntimeDir), "{err}");
        }
    }

    #[test]
    fn preflight_derives_the_bus_only_when_the_socket_exists() {
        let env = ScopeEnv::resolve(true, Some("/run/user/1000"), None, |p| {
            assert_eq!(p, "/run/user/1000/bus");
            true
        })
        .unwrap();
        assert_eq!(env.dbus_address(), "unix:path=/run/user/1000/bus");
    }

    #[test]
    fn preflight_refuses_when_the_bus_socket_is_absent() {
        let err = ScopeEnv::resolve(true, Some("/run/user/1000"), None, |_| false).unwrap_err();
        assert!(
            matches!(err, LaunchError::NoSessionBus(ref d) if d == "/run/user/1000"),
            "{err}"
        );
    }

    #[test]
    fn preflight_prefers_an_inherited_bus_address() {
        let env = ScopeEnv::resolve(
            true,
            Some("/run/user/1000"),
            Some("unix:path=/elsewhere/bus"),
            |_| panic!("must not probe the filesystem when the address is inherited"),
        )
        .unwrap();
        assert_eq!(env.dbus_address(), "unix:path=/elsewhere/bus");
    }

    #[test]
    fn preflight_ignores_an_empty_bus_address() {
        let env = ScopeEnv::resolve(true, Some("/run/user/1000"), Some(""), |_| true).unwrap();
        assert_eq!(env.dbus_address(), "unix:path=/run/user/1000/bus");
    }

    #[test]
    fn there_is_no_unscoped_fallback() {
        // Every preflight failure is an Err. Nothing in this module returns a
        // "launch anyway" variant, because an unscoped launch fails silently at
        // the far end — the case scope launching exists to prevent.
        assert!(ScopeEnv::resolve(false, None, None, |_| false).is_err());
        assert!(ScopeEnv::resolve(true, None, None, |_| false).is_err());
        assert!(ScopeEnv::resolve(true, Some("/x"), None, |_| false).is_err());
    }

    // -- launch confirmation (H1) --------------------------------------------

    /// A `Scope` for a unit name, as `/proc/<pid>/cgroup` would yield.
    fn scope(unit_with_suffix: &str) -> Scope {
        parse_scope_name(unit_with_suffix).unwrap()
    }

    fn never_exits() -> impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>> {
        || Ok(None)
    }

    #[test]
    fn a_launch_is_confirmed_only_once_its_scope_appears() {
        // The scope shows up a few polls in, as it does on a real box.
        let mut polls = 0;
        let ms = confirm(
            "app-steam-app9003-2970",
            4242,
            Duration::from_millis(500),
            never_exits(),
            || {
                polls += 1;
                (polls > 3).then(|| scope("app-steam-app9003-2970.scope"))
            },
            || {},
        )
        .unwrap();
        assert!(ms < 500, "{ms}");
    }

    #[test]
    fn an_unconfirmed_launch_never_becomes_a_launched_payload() {
        // The finding in one assertion: a spawn is not a launch. Every failure
        // `confirm` can return has to come back out of the launch, not be
        // dropped in favour of a payload naming a pid nothing verified.
        for err in [
            LaunchError::NotScoped {
                unit: "app-steam-app9003-2970".into(),
                pid: 4242,
                waited_ms: 2000,
                bound_ms: 2000,
                detail: String::new(),
            },
            LaunchError::ExitedImmediately {
                unit: "app-steam-app9003-2970".into(),
                status: "exit status: 1".into(),
            },
            LaunchError::Confirm(std::io::Error::other("no such process")),
        ] {
            let want = err.to_string();
            let got = finish(AppId::new(9003), 4242, "app-steam-app9003-2970", Err(err))
                .expect_err("an unconfirmed launch must not produce a Launched");
            assert_eq!(got.to_string(), want);
        }
        // And a confirmed one does produce it, with the scope suffix attached.
        let ok = finish(AppId::new(9003), 4242, "app-steam-app9003-2970", Ok(17)).unwrap();
        assert_eq!(ok.scope, "app-steam-app9003-2970.scope");
        assert_eq!(ok.confirmed_ms, 17);
    }

    #[test]
    fn a_launcher_that_exits_immediately_is_not_a_launch() {
        // A misspelled binary, a --unit= collision, a bus that went away after
        // the preflight: systemd-run exits and there is no app. This used to
        // reply with a JSON success payload naming a dead pid.
        use std::os::unix::process::ExitStatusExt as _;
        let err = confirm(
            "app-steam-app9003-2970",
            4242,
            Duration::from_millis(500),
            || Ok(Some(std::process::ExitStatus::from_raw(1 << 8))),
            || panic!("must not ask about the cgroup of a process that has exited"),
            || {},
        )
        .unwrap_err();
        assert!(
            matches!(err, LaunchError::ExitedImmediately { .. }),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("was NOT launched"), "{msg}");
    }

    #[test]
    fn a_live_process_that_never_reaches_its_scope_is_not_a_launch() {
        // The most important one: the process IS alive, so every "is it running"
        // check passes — but gamescope resolves an app id from the cgroup scope
        // alone, so this window could never be focused. Reporting `ok` here is
        // the unscoped launch this module exists to make unreachable.
        let err = confirm(
            "app-steam-app9003-2970",
            4242,
            Duration::from_millis(1),
            never_exits(),
            || None,
            || {},
        )
        .unwrap_err();
        assert!(
            matches!(err, LaunchError::NotScoped { pid: 4242, .. }),
            "{err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("unfocusable"), "{msg}");
    }

    #[test]
    fn landing_in_someone_elses_scope_is_named_in_the_error() {
        let err = confirm(
            "app-steam-app9003-2970",
            4242,
            Duration::from_millis(1),
            never_exits(),
            || Some(scope("app-steam-app770-99.scope")),
            || {},
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("app-steam-app770-99.scope"), "{msg}");
    }

    #[test]
    fn confirmation_never_reports_success_for_a_near_miss() {
        // A scope whose app id differs by one digit is a different app, and the
        // match is on the whole unit name for exactly that reason.
        for other in [
            "app-steam-app9003-2971.scope",
            "app-steam-app90030-2970.scope",
            "app-steam-app903-2970.scope",
        ] {
            let r = confirm(
                "app-steam-app9003-2970",
                1,
                Duration::from_millis(1),
                never_exits(),
                || Some(scope(other)),
                || {},
            );
            assert!(r.is_err(), "{other} must not confirm");
        }
    }

    #[test]
    fn an_unreadable_child_state_is_an_error_not_an_assumed_success() {
        let err = confirm(
            "u",
            1,
            Duration::from_millis(1),
            || Err(std::io::Error::other("no such process")),
            || Some(scope("app-steam-app9003-2970.scope")),
            || {},
        )
        .unwrap_err();
        assert!(matches!(err, LaunchError::Confirm(_)), "{err}");
    }

    // -- PATH resolution (L3) ------------------------------------------------

    #[test]
    fn a_non_executable_file_is_not_a_binary() {
        // Real files rather than a scratch dir: this asserts a property of the
        // filesystem, and a test that has to create a file to state it can fail
        // for reasons (a read-only /tmp, a sandbox) that say nothing about the
        // code. The test binary itself is the executable; this crate's own
        // source is the non-executable regular file.
        let exe = std::env::current_exe().expect("the test binary exists and is executable");
        assert!(is_executable(&exe));

        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(source.is_file(), "{source:?}");
        assert!(
            !is_executable(&source),
            "a readable-but-not-executable file passed the preflight, so the spawn \
             then failed with EACCES instead of the clear 'no systemd-run on PATH'"
        );

        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !is_executable(dir),
            "a DIRECTORY of the right name is not a binary — and directories carry \
             the execute bit as 'searchable', so a mode check without is_file() \
             would accept every one of them"
        );
        assert!(!is_executable(&dir.join("no-such-binary")));
    }

    #[test]
    fn empty_command_is_refused() {
        // Built through the preflight, because there is no other way to build
        // one: the fields are private so a launch cannot skip it.
        let env = ScopeEnv::resolve(true, Some("/run/user/1000"), Some("unix:path=/x"), |_| {
            false
        })
        .unwrap();
        assert!(matches!(
            launch(
                &env,
                AppId::new(1),
                &[],
                LaunchEnv::default(),
                Duration::from_millis(1)
            )
            .unwrap_err(),
            LaunchError::EmptyCommand
        ));
    }

    #[test]
    fn launch_tags_are_unique_within_a_process() {
        let a = next_launch_tag();
        let b = next_launch_tag();
        assert_ne!(a, b, "a relaunch must never collide with a live scope");
    }
}
