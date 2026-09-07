//! The real, X-backed [`Compositor`] — the seam between IPC verbs and gamescope.
//!
//! It holds the X connection, the launch preflight and the config, and turns
//! each verb into the §5 primitive it is: `show`/`home` are one base-layer write
//! plus one bounded verify; `launch` is one scoped `systemd-run` **plus a
//! confirmation that the scope exists and the process is alive**.

use std::sync::Arc;

use crate::atoms::{AppId, AtomConn};
use crate::baselayer::{self, Deadlines, IntentGate};
use crate::config::CoreConfig;
use crate::ipc::Compositor;
use crate::launch::{self, ScopeEnv};
use crate::protocol;
use crate::screen;

/// The live compositor connection.
pub struct GamescopeCompositor {
    conn: AtomConn,
    config: CoreConfig,
    /// `Some` when the scope preflight passed at startup.
    ///
    /// The preflight is done once, not per launch, so a session without a
    /// D-Bus bus fails with one clear message at startup rather than an
    /// identical one on every launch. `None` makes `launch` reply with that
    /// message — never with an unscoped launch, which would appear to succeed
    /// and leave the app unfocusable (see [`crate::launch`]).
    scope_env: Option<ScopeEnv>,
    /// The preflight failure, kept verbatim so the IPC reply says what to fix.
    scope_error: Option<String>,
    /// Serializes one whole intent — the write AND its verify — against others.
    intents: IntentGate,
}

impl GamescopeCompositor {
    /// Connect to X and run the launch preflight.
    ///
    /// A failed preflight is NOT fatal: the core still serves `screen-state`,
    /// `show` and `home`, which is exactly the state in which an operator most
    /// needs a control surface. Only `launch` is refused.
    pub fn connect(config: CoreConfig, display: Option<&str>) -> anyhow::Result<Self> {
        let conn = AtomConn::connect(display)?;
        let (scope_env, scope_error) = match ScopeEnv::detect() {
            Ok(env) => (Some(env), None),
            Err(e) => {
                tracing::warn!("scope launching unavailable: {e}");
                (None, Some(e.to_string()))
            }
        };
        Ok(Self {
            conn,
            config,
            scope_env,
            scope_error,
            intents: IntentGate::new(),
        })
    }

    fn deadlines(&self) -> Deadlines {
        Deadlines {
            switch: self.config.switch_timeout(),
            map: self.config.map_timeout(),
        }
    }

    /// Read the base-layer list back as the core's last intent.
    ///
    /// §9: on start the core is stateless and **never writes "home" on boot** —
    /// that would yank a live game. It observes and reports; it re-asserts only
    /// when something gives it an intent of its own.
    /// Returns what it saw, because the boot client's decision is made from
    /// exactly this observation (see [`crate::boot`]) — and `None` on a failed
    /// read, which that decision treats as "no evidence", never as "empty".
    pub fn reconcile_on_start(&self) -> Option<baselayer::Reconciled> {
        match baselayer::reconcile(&self.conn) {
            Ok(r) => {
                tracing::info!(
                    base_layer = ?r.base_layer,
                    on_screen = ?r.on_screen,
                    "reconciled with the running compositor; not asserting an intent of our own"
                );
                Some(r)
            }
            Err(e) => {
                tracing::warn!("could not read the base layer back: {e}");
                None
            }
        }
    }
}

/// What a launch will actually run: the argv, and the class environment split
/// into the sets and the removals [`launch::LaunchEnv`] takes.
///
/// A named type rather than a tuple because the two env halves are both list-
/// shaped and swapping them would compile — and swapping them means writing
/// `WAYLAND_DISPLAY` instead of removing it, which is the exact bug this change
/// exists to fix.
#[derive(Debug)]
struct ResolvedLaunch<'a> {
    command: &'a [String],
    set: Vec<(String, String)>,
    unset: &'a [String],
}

/// Resolve the argv and environment for a launch.
///
/// The two forms and why both exist:
///
/// * **`launch <appid>`** (empty `command`) — the default path. The class table
///   supplies both the command and the environment, so a caller cannot launch
///   Moonlight while forgetting the four environment operations that stop it
///   selecting native Wayland and never mapping a window.
/// * **`launch <appid> <cmd...>`** — ad-hoc, for a one-off binary or a variant
///   invocation. **It still takes the class environment when the id is a known
///   class**, because the environment is a property of the CLASS, not of the
///   argv: `launch 9003 /usr/bin/moonlight --quit-after` is still Moonlight and
///   still needs `WAYLAND_DISPLAY` gone. An unknown id has no class, so an
///   explicit command runs with no extra environment — that is the escape hatch.
///
/// An unknown id with NO command is the one combination that cannot be served,
/// and it is an `Err` naming what to add. Never a bare exec.
fn resolve_launch<'a>(
    class: Option<&'a crate::config::AppConfig>,
    app_id: AppId,
    command: &'a [String],
) -> Result<ResolvedLaunch<'a>, String> {
    const NO_UNSET: &[String] = &[];
    let owned = |class: &'a crate::config::AppConfig, command: &'a [String]| ResolvedLaunch {
        command,
        set: class.env.clone().into_iter().collect(),
        unset: &class.env_unset,
    };
    match (class, command.is_empty()) {
        (Some(class), true) => Ok(owned(class, &class.command)),
        (Some(class), false) => Ok(owned(class, command)),
        (None, false) => Ok(ResolvedLaunch {
            command,
            set: Vec::new(),
            unset: NO_UNSET,
        }),
        (None, true) => Err(format!(
            "no [[app]] class configured for app id {app_id}; add an [[app]] entry with \
             id = {app_id} to core.toml (command + env), or pass a command explicitly: \
             `launch {app_id} <cmd> [args...]`"
        )),
    }
}

/// The whole `launch` verb, as a function over the preflight result.
///
/// **This is where the no-unscoped-fallback rule would actually be broken.**
/// `ScopeEnv`'s private fields make an unscoped launch unrepresentable *inside*
/// [`crate::launch`], and a test there asserts every preflight branch is an
/// `Err` — but neither says anything about what this layer does with that `Err`.
/// The fallback, if anyone ever wrote one, would be written right here: turning
/// the `None` arm into a plain `Command::new(&command[0])`. So the rule is
/// defended here, by a test that calls this function with `None` and asserts the
/// reply is a refusal naming the reason.
///
/// A free function rather than a method because constructing a
/// [`GamescopeCompositor`] needs a live X server, and a rule whose test needs
/// hardware is a rule with no test.
pub fn launch_reply(
    env: Option<&ScopeEnv>,
    scope_error: Option<&str>,
    confirm_timeout: std::time::Duration,
    app_id: AppId,
    command: &[String],
    class: Option<&crate::config::AppConfig>,
    on_exit: Option<std::sync::mpsc::Sender<std::process::ExitStatus>>,
) -> String {
    let Some(env) = env else {
        // NEVER an unscoped launch. gamescope identifies an app by its cgroup
        // scope, so an unscoped process is invisible to every focus rule: the
        // launch would look like it worked and the app would be unreachable.
        return protocol::resp_error(scope_error.unwrap_or("scope launching is unavailable"));
    };
    let resolved = match resolve_launch(class, app_id, command) {
        Ok(resolved) => resolved,
        Err(why) => return protocol::resp_error(&why),
    };
    let launch_env = launch::LaunchEnv {
        set: &resolved.set,
        unset: resolved.unset,
    };
    match launch::launch(
        env,
        app_id,
        resolved.command,
        launch_env,
        confirm_timeout,
        on_exit,
    ) {
        Ok(launched) => {
            tracing::info!(
                app_id = %app_id,
                pid = launched.pid,
                scope = %launched.scope,
                confirmed_ms = launched.confirmed_ms,
                "launched",
            );
            protocol::resp_json(&launched)
        }
        // §5's rule applied to the launch path: a launch that could not be
        // confirmed is an error, never a success payload naming a dead pid.
        Err(e) => {
            tracing::error!(app_id = %app_id, error = %e, "launch not confirmed");
            protocol::resp_error(&e.to_string())
        }
    }
}

impl Compositor for GamescopeCompositor {
    fn screen_state(&self) -> String {
        match screen::read(&self.conn) {
            Ok(state) => protocol::resp_json(&state),
            Err(e) => protocol::resp_error(&e.to_string()),
        }
    }

    fn show(&self, app_id: AppId) -> String {
        let shell = self.config.shell_app_id();
        let deadlines = self.deadlines();
        // Held across the write AND the verify — see `IntentGate`.
        self.intents.run(|| {
            match baselayer::show(&self.conn, app_id, shell, deadlines) {
                Ok(switched) => {
                    tracing::info!(
                        app_id = %app_id,
                        took_ms = switched.took_ms,
                        waited_for_map_ms = switched.waited_for_map_ms,
                        "base layer switched",
                    );
                    protocol::resp_ok()
                }
                // §5: a mismatch is an error, a metric and a log line, never `ok`.
                Err(e) => {
                    tracing::error!(app_id = %app_id, error = %e, "base-layer switch did not take");
                    protocol::resp_error(&e.to_string())
                }
            }
        })
    }

    fn home(&self) -> String {
        let shell = self.config.shell_app_id();
        let deadlines = self.deadlines();
        self.intents
            .run(|| match baselayer::home(&self.conn, shell, deadlines) {
                Ok(switched) => {
                    tracing::info!(took_ms = switched.took_ms, "returned to the shell");
                    protocol::resp_ok()
                }
                Err(e) => {
                    tracing::error!(error = %e, "return to shell did not take");
                    protocol::resp_error(&e.to_string())
                }
            })
    }

    fn launch(&self, app_id: AppId, command: &[String]) -> String {
        launch_reply(
            self.scope_env.as_ref(),
            self.scope_error.as_deref(),
            self.config.launch_confirm_timeout(),
            app_id,
            command,
            self.config.app_class(app_id),
            None,
        )
    }

    fn launch_supervised(
        &self,
        app_id: AppId,
    ) -> Result<std::sync::mpsc::Receiver<std::process::ExitStatus>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = launch_reply(
            self.scope_env.as_ref(),
            self.scope_error.as_deref(),
            self.config.launch_confirm_timeout(),
            app_id,
            &[],
            self.config.app_class(app_id),
            Some(tx),
        );
        if reply.starts_with("error:") {
            return Err(reply);
        }
        Ok(rx)
    }

    fn on_screen_app(&self) -> Option<AppId> {
        match screen::read(&self.conn) {
            Ok(state) => state.on_screen_app(),
            // A failed read is not "nothing is on screen": the supervisor treats
            // `None` as "the coast is clear", so answering None here would let a
            // relaunch through on no evidence. Report the app we cannot see as
            // *something*, which makes the supervisor yield — fail closed.
            Err(e) => {
                tracing::warn!("could not read what is on screen: {e}");
                Some(crate::boot::SCREEN_UNREADABLE)
            }
        }
    }
}

/// Box a compositor for the IPC server.
pub fn shared(c: GamescopeCompositor) -> Arc<dyn Compositor> {
    Arc::new(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    // -- the app-class table -------------------------------------------------

    fn moonlight_class() -> crate::config::AppConfig {
        crate::config::AppConfig {
            id: 9003,
            command: vec!["/usr/bin/moonlight".to_string()],
            env: [
                ("QT_QPA_PLATFORM".to_string(), "xcb".to_string()),
                ("SDL_VIDEODRIVER".to_string(), "x11".to_string()),
            ]
            .into_iter()
            .collect(),
            env_unset: vec!["WAYLAND_DISPLAY".to_string()],
        }
    }

    /// `launch <appid>` with no command takes BOTH halves from the class.
    #[test]
    fn the_class_form_supplies_the_command_and_the_environment() {
        let class = moonlight_class();
        let r = resolve_launch(Some(&class), AppId::new(9003), &[]).unwrap();
        assert_eq!(r.command, ["/usr/bin/moonlight".to_string()]);
        assert_eq!(r.unset, ["WAYLAND_DISPLAY".to_string()]);
        assert!(r
            .set
            .contains(&("QT_QPA_PLATFORM".to_string(), "xcb".to_string())));
    }

    /// An explicit command for a KNOWN id keeps the class environment.
    ///
    /// The environment belongs to the app class, not to the argv: a variant
    /// invocation of Moonlight is still Moonlight and still fails to map a
    /// window with `WAYLAND_DISPLAY` set. Dropping the env here would make the
    /// ad-hoc form silently different from the one that works, which is the
    /// shape of the bug this whole change is fixing.
    #[test]
    fn an_explicit_command_for_a_known_class_keeps_its_environment() {
        let class = moonlight_class();
        let argv = vec!["/usr/bin/moonlight".to_string(), "--quit-after".to_string()];
        let r = resolve_launch(Some(&class), AppId::new(9003), &argv).unwrap();
        assert_eq!(
            r.command, argv,
            "the explicit argv wins over the class command"
        );
        assert_eq!(
            r.unset,
            ["WAYLAND_DISPLAY".to_string()],
            "but the class environment still applies"
        );
        assert!(!r.set.is_empty());
    }

    /// An UNKNOWN id with an explicit command is the escape hatch: it runs, with
    /// no class environment, because there is no class to take one from.
    #[test]
    fn an_unknown_id_with_a_command_runs_bare() {
        let argv = vec!["/usr/bin/true".to_string()];
        let r = resolve_launch(None, AppId::new(4242), &argv).unwrap();
        assert_eq!(r.command, argv);
        assert!(r.set.is_empty());
        assert!(r.unset.is_empty());
    }

    /// An UNKNOWN id with NO command is a clean error naming what to add — never
    /// a bare exec, and never a silent success.
    ///
    /// Mutation-check: make the `(None, true)` arm fall through to
    /// `Ok((command, ...))` (an empty argv) and this goes red — as does
    /// `launch`'s own `EmptyCommand` guard, which is the second line of defence.
    #[test]
    fn an_unknown_id_with_no_command_is_a_clean_error() {
        let err = resolve_launch(None, AppId::new(4242), &[]).unwrap_err();
        assert!(err.contains("4242"), "{err}");
        assert!(
            err.contains("[[app]]"),
            "the error must name the fix: {err}"
        );

        // And through the whole verb, so the reply an operator sees is checked.
        let reply = launch_reply(
            None,
            Some("preflight failed"),
            Duration::from_millis(1),
            AppId::new(4242),
            &[],
            None,
            None,
        );
        assert!(reply.starts_with("error:"), "{reply}");
        assert!(!reply.contains("\"pid\""), "{reply}");
    }

    /// The class error must survive the reply path on one line, like every other
    /// refusal — it names a config key and is the longest message here.
    #[test]
    fn the_missing_class_refusal_stays_on_one_line() {
        let env = crate::launch::ScopeEnv::resolve(
            true,
            Some("/run/user/1000"),
            Some("unix:path=/run/user/1000/bus"),
            |_| true,
        )
        .unwrap();
        let reply = launch_reply(
            Some(&env),
            None,
            Duration::from_millis(1),
            AppId::new(4242),
            &[],
            None,
            None,
        );
        assert!(reply.starts_with("error:"), "{reply}");
        assert!(!reply.contains('\n'), "{reply:?}");
    }

    /// H3: the place an unscoped fallback would actually be written.
    ///
    /// Mutation-check: replace the `None` arm of `launch_reply` with a plain
    /// spawn and this test goes red.
    #[test]
    fn a_launch_without_a_verified_scope_environment_is_refused() {
        let reply = launch_reply(
            None,
            Some("XDG_RUNTIME_DIR is unset, so `systemd-run --user` has no session bus"),
            Duration::from_millis(1),
            AppId::new(9003),
            &["moonlight".to_string()],
            None,
            None,
        );
        assert!(reply.starts_with("error:"), "{reply}");
        assert!(reply.contains("XDG_RUNTIME_DIR"), "{reply}");
        // A refusal, not a payload: nothing here may look like a launched app.
        assert!(!reply.contains("\"pid\""), "{reply}");
        assert!(!reply.contains("\"scope\""), "{reply}");
    }

    #[test]
    fn a_refusal_still_says_something_when_the_preflight_left_no_message() {
        let reply = launch_reply(
            None,
            None,
            Duration::from_millis(1),
            AppId::new(9003),
            &["moonlight".to_string()],
            None,
            None,
        );
        assert!(reply.starts_with("error:"), "{reply}");
        assert!(reply.len() > "error:".len(), "{reply}");
    }

    #[test]
    fn every_refusal_stays_on_one_line() {
        // The wire protocol is newline-framed; a multi-line refusal desyncs the
        // client. The preflight messages are multi-clause, so this is not idle.
        for why in [
            Some("line one\nline two"),
            Some("DBUS_SESSION_BUS_ADDRESS is unset and /run/user/1000/bus is not a socket"),
            None,
        ] {
            let reply = launch_reply(
                None,
                why,
                Duration::from_millis(1),
                AppId::new(1),
                &["x".to_string()],
                None,
                None,
            );
            assert!(!reply.contains('\n'), "{reply:?}");
        }
    }
}
