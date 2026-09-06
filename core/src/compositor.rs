//! The real, X-backed [`Compositor`] — the seam between IPC verbs and gamescope.
//!
//! It holds the X connection, the launch preflight and the config, and turns
//! each verb into the §5 primitive it is: `show`/`home` are one base-layer write
//! plus one bounded verify; `launch` is one scoped `systemd-run`.

use std::sync::Arc;

use crate::atoms::{AppId, AtomConn};
use crate::baselayer;
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
        })
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

impl Compositor for GamescopeCompositor {
    fn screen_state(&self) -> String {
        match screen::read(&self.conn) {
            Ok(state) => protocol::resp_json(&state),
            Err(e) => protocol::resp_error(&e.to_string()),
        }
    }

    fn show(&self, app_id: AppId) -> String {
        let shell = self.config.shell_app_id();
        match baselayer::show(&self.conn, app_id, shell, self.config.switch_timeout()) {
            Ok(switched) => {
                tracing::info!(app_id = %app_id, took_ms = switched.took_ms, "base layer switched");
                protocol::resp_ok()
            }
            // §5: a mismatch is an error, a metric and a log line, never `ok`.
            Err(e) => {
                tracing::error!(app_id = %app_id, error = %e, "base-layer switch did not take");
                protocol::resp_error(&e.to_string())
            }
        }
    }

    fn home(&self) -> String {
        let shell = self.config.shell_app_id();
        match baselayer::home(&self.conn, shell, self.config.switch_timeout()) {
            Ok(switched) => {
                tracing::info!(took_ms = switched.took_ms, "returned to the shell");
                protocol::resp_ok()
            }
            Err(e) => {
                tracing::error!(error = %e, "return to shell did not take");
                protocol::resp_error(&e.to_string())
            }
        }
    }

    fn launch(&self, app_id: AppId, command: &[String]) -> String {
        let Some(env) = self.scope_env.as_ref() else {
            let why = self
                .scope_error
                .as_deref()
                .unwrap_or("scope launching is unavailable");
            return protocol::resp_error(why);
        };
        match launch::launch(env, app_id, command) {
            Ok(launched) => {
                tracing::info!(app_id = %app_id, pid = launched.pid, scope = %launched.scope, "launched");
                protocol::resp_json(&launched)
            }
            Err(e) => protocol::resp_error(&e.to_string()),
        }
    }
}

/// Box a compositor for the IPC server.
pub fn shared(c: GamescopeCompositor) -> Arc<dyn Compositor> {
    Arc::new(c)
}
