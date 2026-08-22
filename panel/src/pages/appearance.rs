//! `/shell/appearance` — the `Appearance` slice of `settings.json` (theme
//! mode, the two auto-theme hours, reduce-motion, text scale), read and
//! written through the daemon's `get-config`/`set-config` IPC commands.
//!
//! One of the five pages the Settings page dissolved into (`docs/PANEL_IA.md`
//! phase 3). Everything about the schema, the form rendering and the scoped
//! patch lives in [`crate::pages::settings`]; this module is the page and its
//! one save route.
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
/// `__group` companions AND enforced server-side in [`save`], so this route
/// can only ever patch `Appearance`.
const OWNED: &[&str] = &["Appearance"];

#[derive(Template)]
#[template(path = "appearance.html")]
struct AppearanceTemplate {
    chrome: Chrome,
    daemon_up: bool,
    /// One hidden `__group` input per entry — see [`settings::build_patch`].
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
    let tmpl = AppearanceTemplate {
        chrome: Chrome::new(caps, "shell.appearance"),
        daemon_up: cfg.is_some(),
        scope: OWNED.to_vec(),
        groups: cfg
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /shell/appearance/save` — the `Appearance` group only.
///
/// The extractor is a `Vec` of pairs rather than a map because the form emits
/// one `__group` companion per group it owns and a map would collapse them.
pub async fn save(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(render_save(&state, &pairs).await)
}

pub async fn render_save(state: &AppState, pairs: &[(String, String)]) -> String {
    settings::render_save(state, OWNED, pairs).await
}
