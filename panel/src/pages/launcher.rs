//! `/remote/launcher` — what is installed and what was played recently:
//! `list-apps` rendered with a per-app Launch button (`intent app:<wmClass>`),
//! and `get-recents`.
//!
//! One of the four pages the Tools console dissolved into (`docs/PANEL_IA.md`
//! phase 4), and the second half of **Remote**: Navigation moves around inside
//! the shell, Launcher starts something in it. Neither persists anything —
//! what *can* launch is Shell ▸ Apps.
//!
//! Every action funnels through [`crate::pages::ipc_console`], so a
//! daemon-unreachable answer renders as a failed result inline rather than a
//! 500. `GET /remote/launcher` makes no IPC call on load.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::pages::ipc_console::{error_result, esc, result_html, run_line, validate_token};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

#[derive(Template)]
#[template(path = "launcher.html")]
struct LauncherTemplate {
    chrome: Chrome,
}

/// `GET /remote/launcher` — no IPC calls on load; every command is fired by an
/// htmx action.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(LauncherTemplate {
        chrome: Chrome::new(&state.caps, "remote.launcher"),
    })
}

#[derive(Deserialize)]
struct AppEntry {
    name: String,
    #[allow(dead_code)]
    exec: String,
    #[allow(dead_code)]
    icon: String,
    comment: String,
    #[serde(rename = "wmClass")]
    wm_class: String,
}

/// `POST /remote/launcher/list` — `list-apps`, rendered with a per-app
/// "Launch" button (`intent app:<wmClass>`).
pub async fn list_apps(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_list_apps(&state).await)
}

async fn render_list_apps(state: &AppState) -> String {
    match state.node.command_json::<Vec<AppEntry>>("list-apps").await {
        Ok(apps) => {
            if apps.is_empty() {
                return result_html(true, "", "<p class=\"muted\">No launchable apps found.</p>");
            }
            let mut html = String::from(
                r#"<table class="tools-table"><thead><tr><th>Name</th><th>Comment</th><th></th></tr></thead><tbody>"#,
            );
            for a in &apps {
                html.push_str(&format!(
                    r##"<tr><td>{name}</td><td class="muted">{comment}</td><td>
                       <form hx-post="/remote/launcher/launch" hx-disabled-elt="find button" hx-target="#launcher-result" hx-swap="innerHTML" class="inline-form">
                         <input type="hidden" name="wm_class" value="{wm}">
                         <button class="btn-mutate" type="submit">Launch</button>
                       </form></td></tr>"##,
                    name = esc(&a.name),
                    comment = esc(&a.comment),
                    wm = esc(&a.wm_class),
                ));
            }
            html.push_str("</tbody></table>");
            result_html(true, "", &html)
        }
        Err(e) => error_result(&e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct WmClassForm {
    wm_class: String,
}

/// `POST /remote/launcher/launch` — `intent app:<wm_class>`.
pub async fn launch_app(
    State(state): State<SharedState>,
    Form(form): Form<WmClassForm>,
) -> impl IntoResponse {
    Html(render_launch_app(&state, &form.wm_class).await)
}

pub async fn render_launch_app(state: &AppState, wm_class: &str) -> String {
    match validate_token(wm_class) {
        Ok(v) => run_line(state, &format!("intent app:{v}")).await,
        Err(msg) => error_result(&msg),
    }
}

/// `POST /remote/launcher/recents` — `get-recents`.
pub async fn get_recents(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "get-recents").await)
}
