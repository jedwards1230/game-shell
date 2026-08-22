//! `/dev/recovery` — the operator recovery page: deploy/build/restart/reboot/suspend
//! actions. Deploy/build/restart-daemon/restart-shell prefer the daemon HTTP
//! bridge and fall back to direct exec when the bridge is unconfigured or
//! unreachable (deploy has no exec equivalent — it needs the daemon's own
//! git checkout). Reboot/suspend always go through direct exec. All
//! destructive exec calls are single-flighted inside [`crate::exec::Recovery`].

use askama::Template;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::bridge::BridgeError;
use crate::capabilities::{Chrome, Gate};
use crate::config;
use crate::state::{AppState, SharedState};

#[derive(Template)]
#[template(path = "dev.html")]
struct DevTemplate {
    chrome: Chrome,
    daemon_up: bool,
    bridge_configured: bool,
    daemon_chip_html: String,
    shell_chip_html: String,
    /// `[panel].allow_dangerous` (S5). When false the deploy/build/reboot/
    /// suspend forms are not rendered at all — their routes are not registered
    /// either, so rendering them would produce buttons that 404. The two
    /// restart forms are NOT gated by this: restarting a unit is recovery.
    allow_dangerous: bool,
    /// `allow_dangerous` AND the node's `dev_deploy` capability — the exact
    /// pair `build_router` registers `/dev/deploy` + `/dev/build` on. Rendering
    /// those two forms on either half alone would produce buttons that 404.
    deploy_enabled: bool,
    /// The node's `screenshot` capability — same reasoning for the screenshot
    /// panel, whose two routes proxy the daemon bridge's `/screenshot`.
    screenshot_enabled: bool,
}

/// `GET /dev` — probes daemon reachability (bridge `dev_status`, else IPC
/// `status`) and renders the action panel with an up/down banner.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let daemon_up = probe_daemon_up(state).await;
    let daemon_chip_html = render_unit_chip(state, "daemon", config::daemon_unit(), false).await;
    let shell_chip_html = render_unit_chip(state, "shell", config::shell_unit(), false).await;
    let tmpl = DevTemplate {
        chrome: Chrome::new(&state.caps, "dev.recovery"),
        daemon_up,
        bridge_configured: state.cfg.http_bridge_base.is_some(),
        daemon_chip_html,
        shell_chip_html,
        allow_dangerous: state.cfg.allow_dangerous,
        deploy_enabled: state.cfg.allow_dangerous && state.caps.allows(Gate::DevDeploy),
        screenshot_enabled: state.caps.allows(Gate::Screenshot),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// Render a `<span id="dev-{id}-chip">` dot+word status chip for `unit`,
/// using the shared [`super::units::unit_dot`] mapping.
///
/// The dot and its label live inside ONE `.unit-chip` (`white-space:
/// nowrap`): the dot is an inline-block and the label is ordinary text, so
/// without that wrapper the line breaks between them and leaves an orphan dot
/// at the end of the previous line. The id sits on the chip rather than on
/// the dot because an OOB swap replaces the whole element — the label has to
/// come with it.
///
/// `oob` adds `hx-swap-oob="true"` so this can be bolted onto another
/// action's response (#7 — post-action verification: after
/// restart/build/deploy, the operator sees the unit actually came back
/// without a manual page reload) as well as rendered inline on normal page
/// load (`oob = false`).
async fn render_unit_chip(state: &AppState, id: &str, unit: String, oob: bool) -> String {
    let raw = state.recovery.unit_active(&unit).await;
    let (dot_class, word) = super::units::unit_dot(&raw);
    let oob_attr = if oob { " hx-swap-oob=\"true\"" } else { "" };
    format!(
        r#"<span class="unit-chip" id="dev-{id}-chip"{oob_attr} title="{unit}: {raw}"><span class="dot {dot_class}"></span>{id} {word}</span>"#
    )
}

/// Post-action verification (#7): a fresh daemon + shell unit-state chip
/// pair plus a nav-dot refresh, all as htmx out-of-band swaps, appended to
/// every deploy/build/restart-daemon/restart-shell response — so the
/// operator sees the unit(s) actually came back (or didn't) right in the
/// response, instead of waiting on the nav dot's own next ~10s poll or
/// reloading the page.
async fn oob_verification(state: &AppState) -> String {
    let daemon_chip = render_unit_chip(state, "daemon", config::daemon_unit(), true).await;
    let shell_chip = render_unit_chip(state, "shell", config::shell_unit(), true).await;
    let nav_dot = super::nav::render_oob(state).await;
    format!("{daemon_chip}{shell_chip}{nav_dot}")
}

async fn probe_daemon_up(state: &AppState) -> bool {
    if state.bridge.dev_status().await.is_ok() {
        return true;
    }
    state.node.command("status").await.is_ok()
}

#[derive(Template)]
#[template(path = "dev_result.html")]
struct DevResultTemplate {
    tier: &'static str,
    action: &'static str,
    ok: bool,
    output: String,
}

fn result_html(tier: &'static str, action: &'static str, ok: bool, output: &str) -> String {
    let tmpl = DevResultTemplate {
        tier,
        action,
        ok,
        output: output.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `true` when the bridge failure means "no bridge available at all" (so the
/// caller should fall back to direct exec) as opposed to "the bridge is up
/// but the operation itself failed" ([`BridgeError::Status`]).
fn bridge_unavailable(e: &BridgeError) -> bool {
    matches!(e, BridgeError::NotConfigured | BridgeError::Unreachable(_))
}

#[derive(Deserialize)]
pub struct DeployForm {
    git_ref: Option<String>,
}

/// `POST /dev/deploy` — bridge only (no exec equivalent for a git deploy).
pub async fn deploy(
    State(state): State<SharedState>,
    Form(form): Form<DeployForm>,
) -> impl IntoResponse {
    let git_ref = form.git_ref.filter(|s| !s.trim().is_empty());
    Html(render_deploy(&state, git_ref.as_deref()).await)
}

async fn render_deploy(state: &AppState, git_ref: Option<&str>) -> String {
    let result = match state.bridge.deploy(git_ref).await {
        Ok(body) => result_html("Bridge", "deploy", true, &body),
        Err(e) => result_html(
            "Bridge",
            "deploy",
            false,
            &format!(
                "{e} — deploy requires the daemon HTTP bridge (no direct-exec equivalent); \
                 try Restart daemon or Build instead"
            ),
        ),
    };
    format!("{result}{}", oob_verification(state).await)
}

/// `POST /dev/build` — bridge if the daemon is up, else direct exec.
pub async fn build(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_build(&state).await)
}

async fn render_build(state: &AppState) -> String {
    let result = match state.bridge.build().await {
        Ok(body) => result_html("Bridge", "build", true, &body),
        Err(e) if bridge_unavailable(&e) => match state.recovery.build_daemon().await {
            Ok(body) => result_html("Direct exec", "build", true, &body),
            Err(e2) => result_html("Direct exec", "build", false, &e2.to_string()),
        },
        Err(e) => result_html("Bridge", "build", false, &e.to_string()),
    };
    format!("{result}{}", oob_verification(state).await)
}

/// `POST /dev/restart-daemon` — bridge if the daemon is up, else direct exec.
pub async fn restart_daemon(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_restart_daemon(&state).await)
}

async fn render_restart_daemon(state: &AppState) -> String {
    let result = match state.bridge.restart_daemon().await {
        Ok(body) => result_html("Bridge", "restart-daemon", true, &body),
        Err(e) if bridge_unavailable(&e) => match state.recovery.restart_daemon().await {
            Ok(body) => result_html("Direct exec", "restart-daemon", true, &body),
            Err(e2) => result_html("Direct exec", "restart-daemon", false, &e2.to_string()),
        },
        Err(e) => result_html("Bridge", "restart-daemon", false, &e.to_string()),
    };
    format!("{result}{}", oob_verification(state).await)
}

/// `POST /dev/restart-shell` — bridge if the daemon is up, else direct exec.
pub async fn restart_shell(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_restart_shell(&state).await)
}

async fn render_restart_shell(state: &AppState) -> String {
    let result = match state.bridge.restart_shell().await {
        Ok(body) => result_html("Bridge", "restart-shell", true, &body),
        Err(e) if bridge_unavailable(&e) => match state.recovery.restart_shell().await {
            Ok(body) => result_html("Direct exec", "restart-shell", true, &body),
            Err(e2) => result_html("Direct exec", "restart-shell", false, &e2.to_string()),
        },
        Err(e) => result_html("Bridge", "restart-shell", false, &e.to_string()),
    };
    format!("{result}{}", oob_verification(state).await)
}

/// `POST /dev/reboot` — always direct exec.
pub async fn reboot(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_reboot(&state).await)
}

async fn render_reboot(state: &AppState) -> String {
    match state.recovery.reboot().await {
        Ok(body) => result_html("Direct exec", "reboot", true, &body),
        Err(e) => result_html("Direct exec", "reboot", false, &e.to_string()),
    }
}

/// `POST /dev/suspend` — always direct exec.
pub async fn suspend(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_suspend(&state).await)
}

async fn render_suspend(state: &AppState) -> String {
    match state.recovery.suspend().await {
        Ok(body) => result_html("Direct exec", "suspend", true, &body),
        Err(e) => result_html("Direct exec", "suspend", false, &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Screenshot viewer (M3)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "dev_screenshot.html")]
struct DevScreenshotTemplate {
    ok: bool,
    message: String,
    sha: String,
    branch: String,
    version: String,
    captured_at: String,
    cache_bust: u128,
}

/// `POST /dev/screenshot/capture` — calls the bridge screenshot endpoint to
/// confirm reachability and read provenance, then (on success) renders an
/// `<img>` pointing at the `GET /dev/screenshot` proxy route. The `<img>` tag
/// is only ever emitted when this call already succeeded, so a daemon-down
/// or bridge-unconfigured state degrades to a banner — never a broken image.
pub async fn screenshot_capture(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_screenshot_capture(&state).await)
}

pub async fn render_screenshot_capture(state: &AppState) -> String {
    match state.bridge.screenshot().await {
        Ok(shot) => {
            let tmpl = DevScreenshotTemplate {
                ok: true,
                message: String::new(),
                sha: shot.sha,
                branch: shot.branch,
                version: shot.version,
                captured_at: shot.captured_at,
                cache_bust: now_millis(),
            };
            tmpl.render().unwrap_or_else(|e| {
                format!("<p class=\"banner banner-error\">render error: {e}</p>")
            })
        }
        Err(e) => {
            let reason = if e.is_configured() {
                "unreachable"
            } else {
                "not configured"
            };
            let tmpl = DevScreenshotTemplate {
                ok: false,
                message: format!("HTTP bridge {reason} — see the banner above. ({e})"),
                sha: String::new(),
                branch: String::new(),
                version: String::new(),
                captured_at: String::new(),
                cache_bust: 0,
            };
            tmpl.render().unwrap_or_else(|e| {
                format!("<p class=\"banner banner-error\">render error: {e}</p>")
            })
        }
    }
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `GET /dev/screenshot` — proxies the daemon's `GET /screenshot` PNG bytes
/// (`Content-Type: image/png`). Only ever linked from the DOM after
/// [`screenshot_capture`] has already confirmed the bridge is reachable, so
/// a direct hit here (bridge down between the two calls) degrades to a
/// `503` text body rather than corrupting an `<img>` tag's expected type.
pub async fn screenshot_png(State(state): State<SharedState>) -> Response {
    match state.bridge.screenshot().await {
        Ok(shot) => {
            let mut resp = Response::new(Body::from(shot.png));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("image/png"),
            );
            resp
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("screenshot unavailable: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeClient;
    use crate::config::AppConfig;
    use crate::exec::Recovery;
    use crate::ipc::IpcTransport;
    use std::sync::Arc;

    /// Hermetic `AppState` (unreachable IPC socket, no HTTP bridge) — mirrors
    /// `crate::tests::hermetic_state`, duplicated here since that helper lives
    /// in a sibling test module and these `render_*` functions are private to
    /// this page.
    fn hermetic_state() -> Arc<AppState> {
        let sock = std::path::PathBuf::from(format!(
            "/tmp/tvshp-dev-hermetic-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        Arc::new(AppState {
            cfg: AppConfig::default(),
            caps: crate::capabilities::CapabilitySnapshot::fully_capable(),
            node: Arc::new(IpcTransport::new(sock)),
            bridge: Arc::new(BridgeClient::new(None, None)),
            recovery: Recovery::new(),
            updates: crate::updates::UpdatesState::default(),
        })
    }

    /// The dot and its label must ship inside one `.unit-chip`, or the label
    /// wraps to the next line and orphans the dot (`docs/PANEL_IA.md`
    /// § Two known rendering bugs).
    #[tokio::test]
    async fn unit_chip_keeps_the_dot_and_its_label_in_one_nowrap_element() {
        let state = hermetic_state();
        let chip = render_unit_chip(&state, "daemon", "tv-shell-input.service".into(), false).await;
        let chip_open = chip
            .find(r#"<span class="unit-chip""#)
            .expect("a .unit-chip wrapper");
        let dot_open = chip
            .find(r#"<span class="dot "#)
            .expect("a dot span inside the chip");
        assert!(
            chip_open < dot_open && chip.trim_end().ends_with("</span>"),
            "the dot and its label must sit inside one nowrap chip: {chip}"
        );
        assert!(
            chip.contains("daemon "),
            "the chip carries its own label so an OOB swap does not orphan it: {chip}"
        );
    }

    #[tokio::test]
    async fn restart_daemon_response_includes_oob_verification() {
        let state = hermetic_state();
        let html = render_restart_daemon(&state).await;
        assert!(
            html.contains(r#"id="dev-daemon-chip""#) && html.contains(r#"hx-swap-oob="true""#),
            "expected an OOB daemon unit chip refresh: {html}"
        );
        assert!(
            html.contains(r#"id="dev-shell-chip""#),
            "expected an OOB shell unit chip refresh: {html}"
        );
        assert!(
            html.contains(r#"id="daemon-status""#),
            "expected an OOB drawer-footer dot refresh: {html}"
        );
    }

    #[tokio::test]
    async fn restart_shell_build_and_deploy_responses_include_oob_verification() {
        let state = hermetic_state();
        for html in [
            render_restart_shell(&state).await,
            render_build(&state).await,
            render_deploy(&state, None).await,
        ] {
            assert!(
                html.contains(r#"id="dev-daemon-chip""#) && html.contains(r#"id="dev-shell-chip""#),
                "expected both OOB unit chips on every dev action response: {html}"
            );
        }
    }

    #[tokio::test]
    async fn dev_page_renders_inline_unit_chips() {
        let state = hermetic_state();
        let html = render_page(&state).await;
        assert!(
            html.contains(r#"id="dev-daemon-chip""#) && html.contains(r#"id="dev-shell-chip""#),
            "expected the inline unit chips on normal page load: {html}"
        );
    }
}
