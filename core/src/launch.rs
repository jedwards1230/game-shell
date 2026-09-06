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
//!   that socket actually exists.** Exporting a bus address that points at
//!   nothing turns a clear "no session bus" error into a confusing D-Bus timeout.
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

use std::path::Path;
use std::process::{Command, Stdio};

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
}

/// A verified session environment for `systemd-run --user`.
///
/// Constructed only by [`ScopeEnv::detect`], so holding one is proof the
/// preflight passed — a launch cannot skip it by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEnv {
    /// `XDG_RUNTIME_DIR`.
    pub runtime_dir: String,
    /// The session bus address to hand the child, whether inherited or derived.
    pub dbus_address: String,
}

impl ScopeEnv {
    /// Run the preflight against the process environment.
    pub fn detect() -> Result<Self, LaunchError> {
        Self::resolve(
            which(SYSTEMD_RUN),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
            std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
            |p| Path::new(p).exists(),
        )
    }

    /// The pure half of the preflight — no environment, no filesystem.
    ///
    /// `bus_socket_exists` answers "is `<runtime_dir>/bus` there?"; in production
    /// that is a stat, in tests it is a closure, which is what makes every branch
    /// of the fail-closed policy testable without a session bus.
    pub fn resolve(
        has_systemd_run: bool,
        runtime_dir: Option<&str>,
        dbus_address: Option<&str>,
        bus_socket_exists: impl Fn(&str) -> bool,
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
                if bus_socket_exists(&candidate) {
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
    format!("app-steam-app{}-{}", app_id.0, launcher_pid)
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

/// A launched app: the pid that owns the scope, and the scope's name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Launched {
    pub app_id: AppId,
    pub pid: u32,
    /// The unit name **with** its `.scope` suffix, as it appears in cgroup paths.
    pub scope: String,
}

/// Launch `command` inside a transient `app-steam-app<appid>-<pid>.scope`.
///
/// The child is spawned and NOT waited on: the core supervises it by unit, and
/// blocking here would stall the IPC reactor. stdin is `/dev/null`; stdout and
/// stderr are inherited so the child's output lands in the core's journal
/// alongside `systemd-run`'s `Running as unit:` line.
///
/// Note the pid recorded is `systemd-run`'s own. That is correct and load-
/// bearing: `systemd-run --scope` **execs** the target command, so the pid the
/// caller holds is the app's own pid the whole way down (verified by the kit),
/// which keeps pid-matching, family walks and `wait` working unchanged.
pub fn launch(env: &ScopeEnv, app_id: AppId, command: &[String]) -> Result<Launched, LaunchError> {
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

    let child = Command::new(&argv[0])
        .args(&argv[1..])
        .env("XDG_RUNTIME_DIR", &env.runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", &env.dbus_address)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|source| LaunchError::Spawn {
            unit: unit.clone(),
            source,
        })?;

    Ok(Launched {
        app_id,
        pid: child.id(),
        scope: format!("{unit}.scope"),
    })
}

/// A per-process monotonic tag for the scope name's second field.
///
/// gamescope reads it as an opaque `%d` and never resolves it to a process, so
/// its only job is to make the unit name unique per launch — the property the
/// shell version got from `$BASHPID`. Seeding from our own pid keeps two cores
/// (a restart mid-flight, a dev copy) from minting the same name.
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
        app_id: AppId(app.parse().ok()?),
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

/// Is `name` on `PATH`?
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real cgroup v2 body, in the shape `/proc/<pid>/cgroup` actually has on
    /// a systemd user session: one `0::` line, full unit path.
    const REAL_CGROUP: &str =
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steam-app9003-2970.scope\n";

    #[test]
    fn unit_name_matches_the_upstream_grammar() {
        assert_eq!(scope_unit(AppId(9003), 2970), "app-steam-app9003-2970");
        assert_eq!(scope_unit(AppId(0), 1), "app-steam-app0-1");
    }

    #[test]
    fn unit_name_round_trips_through_the_parser() {
        let unit = scope_unit(AppId(413091), 12345);
        let scope = parse_scope_name(&format!("{unit}.scope")).unwrap();
        assert_eq!(scope.app_id, AppId(413091));
        assert_eq!(scope.tag, 12345);
    }

    #[test]
    fn parses_a_real_cgroup_body() {
        let scope = parse_cgroup_scope(REAL_CGROUP).unwrap();
        assert_eq!(scope.app_id, AppId(9003));
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
        assert_eq!(parse_cgroup_scope(body).unwrap().app_id, AppId(770));
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
        assert_eq!(env.dbus_address, "unix:path=/run/user/1000/bus");
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
        assert_eq!(env.dbus_address, "unix:path=/elsewhere/bus");
    }

    #[test]
    fn preflight_ignores_an_empty_bus_address() {
        let env = ScopeEnv::resolve(true, Some("/run/user/1000"), Some(""), |_| true).unwrap();
        assert_eq!(env.dbus_address, "unix:path=/run/user/1000/bus");
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

    #[test]
    fn empty_command_is_refused() {
        let env = ScopeEnv {
            runtime_dir: "/run/user/1000".into(),
            dbus_address: "unix:path=/x".into(),
        };
        assert!(matches!(
            launch(&env, AppId(1), &[]).unwrap_err(),
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
