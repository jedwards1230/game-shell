//! `/remote/navigation` — driving the running shell from here: intents
//! (`home`/`menu`/`settings`/`power`), the overlay quick actions, the settings
//! deep-links, and the six-key D-pad vocabulary.
//!
//! One of the four pages the Tools console dissolved into (`docs/PANEL_IA.md`
//! phase 4). Nothing on this page persists anything — these are transient
//! commands that change what is on screen right now, which is what makes
//! **Remote** a group of its own rather than more Shell configuration.
//!
//! Every action funnels through [`crate::pages::ipc_console`], so a
//! daemon-unreachable answer renders as a failed result inline rather than a
//! 500. `GET /remote/navigation` makes no IPC call on load.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::pages::ipc_console::{error_result, run_line, validate_token};
use crate::state::{AppState, SharedState};

const INTENT_QUICK: &[&str] = &["home", "home-tap", "home-hold", "menu", "settings", "power"];
const OVERLAY_QUICK: &[&str] = &["overlay:volume", "overlay:network", "overlay:session"];
/// Settings page slugs (`docs/CONTROL_SURFACE.md` § Intent vocabulary).
const SETTINGS_SLUGS: &[&str] = &[
    "audio",
    "bluetooth",
    "network",
    "display",
    "controllers",
    "keybindings",
    "avcontrol",
    "widgets",
    "accessibility",
    "power",
    "system",
];
/// The closed key vocabulary — validated server-side too, as defense in depth
/// beyond the fixed button values.
const KEY_VOCAB: &[&str] = &["up", "down", "left", "right", "select", "back"];

#[derive(Template)]
#[template(path = "navigation.html")]
struct NavigationTemplate {
    chrome: Chrome,
    intent_quick: &'static [&'static str],
    overlay_quick: &'static [&'static str],
    settings_slugs: &'static [&'static str],
    key_quick: &'static [&'static str],
}

/// `GET /remote/navigation` — no IPC calls on load; every command is fired by
/// an htmx action.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(NavigationTemplate {
        chrome: Chrome::new(&state.caps, "remote.navigation"),
        intent_quick: INTENT_QUICK,
        overlay_quick: OVERLAY_QUICK,
        settings_slugs: SETTINGS_SLUGS,
        key_quick: KEY_VOCAB,
    })
}

#[derive(Deserialize)]
pub struct NameForm {
    name: String,
}

/// `POST /remote/navigation/intent` — the free-text field and every
/// quick/deep-link button funnel through here (`name` is the intent's
/// `<name>` argument).
pub async fn intent(
    State(state): State<SharedState>,
    Form(form): Form<NameForm>,
) -> impl IntoResponse {
    Html(render_intent(&state, &form.name).await)
}

pub async fn render_intent(state: &AppState, name: &str) -> String {
    match validate_token(name) {
        Ok(v) => run_line(state, &format!("intent {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

/// `POST /remote/navigation/key` — the six-name closed vocabulary.
pub async fn key(
    State(state): State<SharedState>,
    Form(form): Form<NameForm>,
) -> impl IntoResponse {
    Html(render_key(&state, &form.name).await)
}

pub async fn render_key(state: &AppState, name: &str) -> String {
    let t = name.trim();
    if !KEY_VOCAB.contains(&t) {
        return error_result(&format!(
            "unknown key {t:?} (allowed: {})",
            KEY_VOCAB.join(", ")
        ));
    }
    run_line(state, &format!("key {t}")).await
}
