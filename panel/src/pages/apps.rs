//! `/shell/apps` — the `Apps` slice of `settings.json`: the `prewarmApps`
//! list editor (one `StartupWMClass` per line), read and written through the
//! daemon's `get-config`/`set-config` IPC commands.
//!
//! One of the five pages the Settings page dissolved into (`docs/PANEL_IA.md`
//! phase 3). Phase 4 folds the Media page's web-app registry in here, at
//! which point "what can launch on this box" is one page.
//!
//! Degradation: with the daemon unreachable the page still returns 200 with a
//! clear banner and no form — never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde_json::Value;

use crate::capabilities::{CapabilitySnapshot, Chrome};
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page owns — rendered as the form's
/// `__group` companions AND enforced server-side in [`save`].
const OWNED: &[&str] = &["Apps"];

#[derive(Template)]
#[template(path = "apps.html")]
struct AppsTemplate {
    chrome: Chrome,
    daemon_up: bool,
    scope: Vec<&'static str>,
    groups: Vec<GroupView>,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    render(&state.caps, state.node.get_config().await.ok().as_ref())
}

fn render(caps: &CapabilitySnapshot, cfg: Option<&Value>) -> String {
    let tmpl = AppsTemplate {
        chrome: Chrome::new(caps, "shell.apps"),
        daemon_up: cfg.is_some(),
        scope: OWNED.to_vec(),
        groups: cfg
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /shell/apps/save` — the `Apps` group only.
pub async fn save(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(render_save(&state, &pairs).await)
}

pub async fn render_save(state: &AppState, pairs: &[(String, String)]) -> String {
    settings::render_save(state, OWNED, pairs).await
}
