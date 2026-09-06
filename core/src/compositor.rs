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
    pub fn reconcile_on_start(&self) {
        match baselayer::reconcile(&self.conn) {
            Ok(r) => tracing::info!(
                base_layer = ?r.base_layer,
                on_screen = ?r.on_screen,
                "reconciled with the running compositor; not asserting an intent of our own"
            ),
            Err(e) => tracing::warn!("could not read the base layer back: {e}"),
        }
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
) -> String {
    let Some(env) = env else {
        // NEVER an unscoped launch. gamescope identifies an app by its cgroup
        // scope, so an unscoped process is invisible to every focus rule: the
        // launch would look like it worked and the app would be unreachable.
        return protocol::resp_error(scope_error.unwrap_or("scope launching is unavailable"));
    };
    match launch::launch(env, app_id, command, confirm_timeout) {
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
        )
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
            );
            assert!(!reply.contains('\n'), "{reply:?}");
        }
    }
}
