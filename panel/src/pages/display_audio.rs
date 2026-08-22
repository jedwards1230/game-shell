//! `/devices/display-audio` — the `Display`, `Night Light`, `Power` and
//! `Audio` slices of `settings.json`, read and written through the daemon's
//! `get-config`/`set-config` IPC commands.
//!
//! One of the five pages the Settings page dissolved into (`docs/PANEL_IA.md`
//! phase 3): four groups that all describe the picture and sound coming out of
//! the box, on one page instead of buried in a 3133px scroll. Phase 4 added
//! the Tools page's two power probes (`power-can-suspend`, `power-battery`)
//! beside the `Power` settings group they report on.
//!
//! **Two gates on one page.** The settings form is [`Gate::SettingsStore`]
//! (which is also where the page itself is registered); the two probe routes
//! are [`Gate::Node`], because they are IPC reads that map to no declared
//! `Feature`. The probe buttons therefore render only under
//! `caps.allows(Gate::Node)` — otherwise the page would offer two buttons
//! POSTing to unregistered routes.
//!
//! Degradation: with the daemon unreachable the page still returns 200 with a
//! clear banner and no form — never a 500; the probes report the transport
//! error inline.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde_json::Value;

use crate::capabilities::{CapabilitySnapshot, Chrome, Gate};
use crate::pages::ipc_console::run_line;
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page owns — one `__group` companion each,
/// AND enforced server-side in [`save`]. Four groups on one form is exactly
/// the case that makes the companions repeatable rather than a single value.
const OWNED: &[&str] = &["Display", "Night Light", "Power", "Audio"];

#[derive(Template)]
#[template(path = "display_audio.html")]
struct DisplayAudioTemplate {
    chrome: Chrome,
    daemon_up: bool,
    scope: Vec<&'static str>,
    groups: Vec<GroupView>,
    /// The node tier — see the module docs. Gates the two probe buttons, whose
    /// routes are registered in the `Gate::Node` block rather than this page's.
    node_enabled: bool,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    render(&state.caps, state.node.get_config().await.ok().as_ref())
}

fn render(caps: &CapabilitySnapshot, cfg: Option<&Value>) -> String {
    let tmpl = DisplayAudioTemplate {
        chrome: Chrome::new(caps, "devices.display-audio"),
        daemon_up: cfg.is_some(),
        scope: OWNED.to_vec(),
        groups: cfg
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
        node_enabled: caps.allows(Gate::Node),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /devices/display-audio/save` — the four groups above and nothing
/// else. In particular the `Appearance`, `Input`, `CEC` and `Apps` bools on
/// the other four pages are left untouched rather than written `false`.
pub async fn save(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(render_save(&state, &pairs).await)
}

pub async fn render_save(state: &AppState, pairs: &[(String, String)]) -> String {
    settings::render_save(state, OWNED, pairs).await
}

// ---------------------------------------------------------------------------
// Power probes (node tier — from the dissolved Tools page)
// ---------------------------------------------------------------------------

/// `POST /devices/display-audio/power/can-suspend` — `power-can-suspend`.
pub async fn power_can_suspend(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "power-can-suspend").await)
}

/// `POST /devices/display-audio/power/battery` — `power-battery`.
pub async fn power_battery(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "power-battery").await)
}
