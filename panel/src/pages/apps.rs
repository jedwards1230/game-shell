//! `/shell/apps` — what can launch on this box: the `Apps` slice of
//! `settings.json` (the `prewarmApps` list, one `StartupWMClass` per line),
//! and the daemon-owned **web-app registry**.
//!
//! One of the five pages the Settings page dissolved into (`docs/PANEL_IA.md`
//! phase 3); phase 4 folded the Media page's web-app half in here, so the two
//! answers to "what can start on this box" finally share a page. Everything
//! about the settings schema, the form rendering and the scoped patch lives in
//! [`crate::pages::settings`].
//!
//! ## Web apps
//!
//! Add/remove entries in the daemon-owned registry (`webapp-add`/
//! `webapp-remove`/`webapp-list`, #187 P1+P3). `docs/WEB_APPS.md` deferred the
//! shell-side add flow because the couch UI has no on-screen keyboard (#20);
//! the panel has a real keyboard, so it owns the add flow. The daemon
//! validates, allocates the id/`wmClass` and writes the `.desktop` file — the
//! panel only relays.
//!
//! Degradation: with the daemon unreachable the page still returns 200 with a
//! clear banner and no form — never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::capabilities::{CapabilitySnapshot, Chrome, Gate};
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page owns — rendered as the form's
/// `__group` companions AND enforced server-side in [`save`].
const OWNED: &[&str] = &["Apps"];

struct WebAppView {
    id: String,
    name: String,
    url: String,
    wm_class: String,
}

#[derive(Template)]
#[template(path = "apps.html")]
struct AppsTemplate {
    chrome: Chrome,
    daemon_up: bool,
    scope: Vec<&'static str>,
    groups: Vec<GroupView>,
    webapps: Vec<WebAppView>,
    webapps_error: String,
    /// The node's `web_apps` capability — the add/remove routes sit in that
    /// block while this page sits in the `settings_store` one, so a node that
    /// declares the latter but not the former would otherwise be rendered
    /// forms POSTing to unregistered routes.
    webapps_enabled: bool,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let cfg = state.node.get_config().await.ok();
    let (webapps, webapps_error) = match state.node.command("webapp-list").await {
        Ok(reply) => (parse_webapps(&reply), String::new()),
        Err(e) => (Vec::new(), format!("Could not read the registry: {e}")),
    };
    render(&state.caps, cfg.as_ref(), webapps, webapps_error)
}

fn render(
    caps: &CapabilitySnapshot,
    cfg: Option<&Value>,
    webapps: Vec<WebAppView>,
    webapps_error: String,
) -> String {
    let tmpl = AppsTemplate {
        chrome: Chrome::new(caps, "shell.apps"),
        daemon_up: cfg.is_some(),
        scope: OWNED.to_vec(),
        groups: cfg
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
        webapps,
        webapps_error,
        webapps_enabled: caps.allows(Gate::WebApps),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn parse_webapps(reply: &str) -> Vec<WebAppView> {
    serde_json::from_str::<serde_json::Value>(reply)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|v| WebAppView {
            id: v
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            url: v
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            wm_class: v
                .get("wmClass")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .filter(|a| !a.id.is_empty())
        .collect()
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

// ---------------------------------------------------------------------------
// Web apps — add / remove
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "action_result.html")]
struct ActionResultTemplate {
    ok: bool,
    message: String,
}

fn result_html(ok: bool, message: &str) -> String {
    let tmpl = ActionResultTemplate {
        ok,
        message: message.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// htmx result + an out-of-band refresh of the registry table, so a successful
/// add/remove updates the page without a full reload.
fn result_with_refresh(ok: bool, message: &str, refreshed: String) -> String {
    format!("{}{}", result_html(ok, message), refreshed)
}

#[derive(Deserialize)]
pub struct WebAppForm {
    name: String,
    url: String,
}

/// `POST /shell/apps/webapp/add` — the daemon validates, allocates the
/// id/wmClass, writes the `.desktop`, and owns the registry; the panel just
/// relays.
pub async fn webapp_add(
    State(state): State<SharedState>,
    Form(form): Form<WebAppForm>,
) -> impl IntoResponse {
    Html(render_webapp_add(&state, &form.name, &form.url).await)
}

pub async fn render_webapp_add(state: &AppState, name: &str, url: &str) -> String {
    let body = json!({ "name": name.trim(), "url": url.trim() });
    let line = format!("webapp-add {body}");
    match state.node.command(&line).await {
        Ok(reply) => {
            let added: Option<String> = serde_json::from_str::<serde_json::Value>(&reply)
                .ok()
                .and_then(|v| {
                    v.get("wmClass")
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                });
            let msg = match added {
                Some(wm) => format!(
                    "Added {}. It appears on the home Applications row as {wm} \
                     (launcher written to ~/.local/share/applications).",
                    name.trim()
                ),
                None => format!("Added {}.", name.trim()),
            };
            let refreshed = render_webapp_list_oob(state).await;
            result_with_refresh(true, &msg, refreshed)
        }
        Err(e) => result_html(false, &format!("Could not add the web app: {e}")),
    }
}

#[derive(Deserialize)]
pub struct IdForm {
    id: String,
}

/// `POST /shell/apps/webapp/remove` — drop a registry entry and its launcher.
pub async fn webapp_remove(
    State(state): State<SharedState>,
    Form(form): Form<IdForm>,
) -> impl IntoResponse {
    Html(render_webapp_remove(&state, &form.id).await)
}

pub async fn render_webapp_remove(state: &AppState, id: &str) -> String {
    match state
        .node
        .command(&format!("webapp-remove {}", id.trim()))
        .await
    {
        Ok(_) => {
            let refreshed = render_webapp_list_oob(state).await;
            result_with_refresh(
                true,
                &format!(
                    "Removed {id}. Its Chromium profile was kept, so re-adding restores logins."
                ),
                refreshed,
            )
        }
        Err(e) => result_html(false, &format!("Could not remove the web app: {e}")),
    }
}

/// The registry table as an out-of-band htmx swap.
///
/// Re-renders THIS page and string-slices the table out of it between the two
/// HTML comment markers `apps.html` carries — see
/// [`crate::pages::appearance::render_wallpaper_list_oob`] for why, and
/// `crate::tests::webapp_oob_refresh_returns_the_list_fragment` for the pin.
/// `pub` for that test: a silently-empty fragment is the exact failure mode.
pub async fn render_webapp_list_oob(state: &AppState) -> String {
    let inner = render_page(state).await;
    match (
        inner.find("<!--webapp-list-start-->"),
        inner.find("<!--webapp-list-end-->"),
    ) {
        (Some(a), Some(b)) if b > a => format!(
            r#"<div id="webapp-list" hx-swap-oob="innerHTML">{}</div>"#,
            &inner[a..b]
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_webapps_is_lenient() {
        let good = r#"[{"id":"yt","name":"YouTube","url":"https://y.tv","wmClass":"tvshell-yt"}]"#;
        let apps = parse_webapps(good);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].wm_class, "tvshell-yt");
        assert!(parse_webapps("not json").is_empty());
        assert!(parse_webapps("{}").is_empty());
        // Entries without an id are dropped rather than rendered blank.
        assert!(parse_webapps(r#"[{"name":"x"}]"#).is_empty());
    }
}
