//! `/system/services` — the systemd unit table and its per-unit restart.
//!
//! Split out of the old Processes page (`docs/PANEL_IA.md` phase 2): unit
//! *control* is a different job from process *observation*, and it is the one
//! surface on the page that mutates anything.
//!
//! Scope in this phase is deliberately the three built-in tv-shell user units
//! (daemon/shell/panel) and nothing else. Reading arbitrary units and the
//! configurable `managed_units` restart allowlist are phase 5
//! (`docs/PANEL_IA.md` § Services (new)) — the built-ins stay hardcoded there
//! too, so a config typo can never cost the recovery path.
//!
//! Degradation: everything here is exec-based (`systemctl`), so it works with
//! the daemon down. That is the whole point — this is a recovery surface.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};

use crate::capabilities::Chrome;
use crate::config;
use crate::state::{AppState, SharedState};

use super::units::unit_dot;

struct UnitView {
    key: &'static str,
    label: &'static str,
    unit: String,
    state: String,
    /// A dedicated dot/word status pair (color always paired with explicit
    /// text — #6), from the shared `pages::units::unit_dot` that Overview's
    /// Units tile and the Dev page's chips also render.
    dot_class: &'static str,
    state_word: &'static str,
    /// Confirm-dialog text for this unit's Restart button. The panel's own
    /// unit gets a distinct message (#5): restarting it drops the very page
    /// the operator is looking at, so the confirm says so explicitly rather
    /// than reusing the generic "Restart X now?" wording.
    confirm: String,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesTemplate {
    chrome: Chrome,
    units: Vec<UnitView>,
}

/// `GET /system/services`.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let units = vec![
        unit_view(state, "daemon", "Daemon", config::daemon_unit()).await,
        unit_view(state, "shell", "Shell", config::shell_unit()).await,
        unit_view(state, "panel", "Panel", config::panel_unit()).await,
    ];

    let tmpl = ServicesTemplate {
        chrome: Chrome::new(&state.caps, "system.services"),
        units,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

async fn unit_view(
    state: &AppState,
    key: &'static str,
    label: &'static str,
    unit: String,
) -> UnitView {
    let unit_state = state.recovery.unit_active(&unit).await;
    let (dot_class, state_word) = unit_dot(&unit_state);
    let confirm = if key == "panel" {
        format!(
            "Restart {unit} now? This is the panel serving THIS page — it will disconnect \
             immediately. Reload the page after a few seconds to reconnect."
        )
    } else {
        format!("Restart {unit} now?")
    };
    UnitView {
        key,
        label,
        unit,
        state: unit_state,
        dot_class,
        state_word,
        confirm,
    }
}

#[derive(Template)]
#[template(path = "services_result.html")]
struct ServicesResultTemplate {
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = ServicesResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /system/services/restart/{key}` — restart one of the three tv-shell
/// units. `key` is matched against a fixed set (`daemon`/`shell`/`panel`) and
/// resolved to the real unit name server-side — never an arbitrary
/// client-supplied unit name reaches `systemctl`. `docs/PANEL_IA.md`
/// § "Preserving the no-arbitrary-unit property" pins this for phase 5 too:
/// the key is an index into a server-side table, not a unit name passed
/// through.
pub async fn restart(
    State(state): State<SharedState>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    Html(render_restart(&state, &key).await)
}

pub async fn render_restart(state: &AppState, key: &str) -> String {
    let unit = match key {
        "daemon" => config::daemon_unit(),
        "shell" => config::shell_unit(),
        "panel" => config::panel_unit(),
        other => return result_html(false, &format!("unknown unit key {other:?}")),
    };
    match state.recovery.restart_unit(&unit).await {
        Ok(out) => result_html(true, &format!("restarted {unit}\n{out}")),
        Err(e) => result_html(false, &format!("restart {unit} failed: {e}")),
    }
}
