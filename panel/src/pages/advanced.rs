//! `/shell/advanced` — the escape hatches, quarantined behind one deliberate
//! click (`docs/PANEL_IA.md` phase 3):
//!
//! - the daemon-owned key subtrees, **read-only**;
//! - the `config.toml` view, **read-only**;
//! - the raw-JSON hatch, which can write *any* key in `settings.json`
//!   including the daemon-owned binding layers, and where a `null` deletes.
//!
//! That last one is why this page exists. It used to sit directly below the
//! ordinary typed toggles on the old Settings page's single scroll, with a
//! paragraph between them; now nothing on this page is an ordinary toggle.
//!
//! Degradation: with the daemon unreachable the page still returns 200 — the
//! two `settings.json`-backed blocks disappear behind an honest note, and the
//! `config.toml` view (which this page owns, and which is read straight off
//! the panel's own filesystem) still works.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;
use serde_json::Value;

use crate::capabilities::{CapabilitySnapshot, Chrome};
use crate::pages::settings;
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

#[derive(Template)]
#[template(path = "advanced.html")]
struct AdvancedTemplate {
    chrome: Chrome,
    daemon_up: bool,
    complex_notes_html: String,
    daemon_owned_json: String,
    config_toml: String,
    config_toml_path: String,
    raw_json: String,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    render(&state.caps, state.node.get_config().await.ok().as_ref())
}

fn render(caps: &CapabilitySnapshot, cfg: Option<&Value>) -> String {
    let (config_toml, config_toml_path) = settings::read_config_toml();
    let tmpl = AdvancedTemplate {
        chrome: Chrome::new(caps, "shell.advanced"),
        daemon_up: cfg.is_some(),
        complex_notes_html: cfg
            .map(|_| settings::complex_notes_html())
            .unwrap_or_default(),
        daemon_owned_json: cfg.map(settings::daemon_owned_json).unwrap_or_default(),
        config_toml,
        config_toml_path,
        // Pretty-printed for editing; `render_save_raw` accepts this unchanged
        // (`serde_json::from_str` tolerates whitespace) and
        // `NodeTransportExt::set_config` compacts it back to a single line
        // before it ever reaches the daemon.
        raw_json: cfg
            .map(|c| serde_json::to_string_pretty(c).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_default(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

#[derive(Deserialize)]
pub struct RawForm {
    raw_json: String,
}

/// `POST /shell/advanced/raw` — validates the submitted text is a JSON
/// *object* server-side before writing anything (client-side JS in
/// `advanced.html` does the same check for immediate feedback, but this is the
/// authoritative gate). A parse failure or non-object body returns a 200 error
/// partial; nothing is sent to the daemon in either case.
pub async fn save_raw(
    State(state): State<SharedState>,
    Form(form): Form<RawForm>,
) -> impl IntoResponse {
    Html(render_save_raw(&state, &form.raw_json).await)
}

pub async fn render_save_raw(state: &AppState, raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => match state.node.set_config(&v).await {
            Ok(()) => settings::result_html(true, "Raw JSON merged into settings.json."),
            Err(e) => settings::result_html(false, &format!("Save failed: {e}")),
        },
        Ok(_) => settings::result_html(
            false,
            "Invalid: raw JSON must be an object, e.g. {\"key\":value} — not an array or scalar.",
        ),
        Err(e) => settings::result_html(false, &format!("Invalid JSON: {e}")),
    }
}
