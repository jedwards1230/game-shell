//! `/devices/network` — the two radios attached to the box: NetworkManager
//! (link status, Wi-Fi scan, per-interface throughput, ping) and bluez
//! (adapter power, discovery, the known-device list and its per-device
//! connect/disconnect/pair/trust actions).
//!
//! One of the four pages the Tools console dissolved into (`docs/PANEL_IA.md`
//! phase 4). Network and Bluetooth were two sections of a page grouped by IPC
//! domain; here they are one page grouped by *subject* — "the box's network
//! interfaces", which is what an operator is actually looking for.
//!
//! Every action funnels through [`crate::pages::ipc_console`], so a
//! daemon-unreachable answer renders as a failed result inline rather than a
//! 500. `GET /devices/network` makes no IPC call on load, so the page itself
//! is always 200 with the full console rendered.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::pages::ipc_console::{
    error_result, esc, result_html, run_line, validate_iface, validate_token,
};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The per-device actions the Bluetooth table renders, and the closed set
/// `bt_action` validates against server-side.
const BT_ACTIONS: &[&str] = &["connect", "disconnect", "pair", "trust"];

#[derive(Template)]
#[template(path = "network.html")]
struct NetworkTemplate {
    chrome: Chrome,
}

/// `GET /devices/network` — no IPC calls on load; every command is fired by an
/// htmx action.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(NetworkTemplate {
        chrome: Chrome::new(&state.caps, "devices.network"),
    })
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

pub async fn status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-status").await)
}

pub async fn wifi_list(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-wifi-list").await)
}

pub async fn wifi_rescan(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-wifi-rescan").await)
}

#[derive(Deserialize)]
pub struct IfaceForm {
    iface: String,
}

/// `POST /devices/network/throughput` — `net-throughput <iface>`. `iface` is
/// validated as a plain token with no path separators (it touches a sysfs
/// path on the daemon side).
pub async fn throughput(
    State(state): State<SharedState>,
    Form(form): Form<IfaceForm>,
) -> impl IntoResponse {
    Html(render_throughput(&state, &form.iface).await)
}

pub async fn render_throughput(state: &AppState, iface: &str) -> String {
    match validate_iface(iface) {
        Ok(v) => run_line(state, &format!("net-throughput {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

#[derive(Deserialize)]
pub struct PingForm {
    host: String,
    count: Option<String>,
}

/// `POST /devices/network/ping` — `net-ping <host> [count]`. `host` is
/// validated as a single token (no whitespace/control chars — it becomes an
/// argv, not a shell string, but a stray space would still split the daemon's
/// own whitespace-delimited command parsing); `count`, if given, must be an
/// integer in `1..=10` (the daemon clamps too, but reject out-of-range input
/// here rather than silently reinterpreting it).
pub async fn ping(
    State(state): State<SharedState>,
    Form(form): Form<PingForm>,
) -> impl IntoResponse {
    Html(render_ping(&state, &form.host, form.count.as_deref()).await)
}

pub async fn render_ping(state: &AppState, host: &str, count: Option<&str>) -> String {
    let h = match validate_token(host) {
        Ok(v) => v,
        Err(msg) => return error_result(&msg),
    };
    let line = match count.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => match c.parse::<u32>() {
            Ok(n) if (1..=10).contains(&n) => format!("net-ping {h} {n}"),
            _ => return error_result("count must be an integer between 1 and 10"),
        },
        None => format!("net-ping {h}"),
    };
    run_line(state, &line).await
}

// ---------------------------------------------------------------------------
// Bluetooth
// ---------------------------------------------------------------------------

pub async fn bt_power_status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "bt-power-status").await)
}

pub async fn bt_power_on(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "bt-power-on").await)
}

pub async fn bt_power_off(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "bt-power-off").await)
}

pub async fn bt_scan_on(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "bt-scan-on").await)
}

pub async fn bt_scan_off(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "bt-scan-off").await)
}

#[derive(Deserialize)]
struct BtDevice {
    mac: String,
    name: Option<String>,
    paired: bool,
    connected: bool,
    trusted: bool,
    #[allow(dead_code)]
    rssi: Option<i64>,
}

/// `POST /devices/network/bt/list` — `bt-list`, rendered with per-device
/// connect/disconnect/pair/trust actions.
pub async fn bt_list(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_bt_list(&state).await)
}

async fn render_bt_list(state: &AppState) -> String {
    match state.node.command_json::<Vec<BtDevice>>("bt-list").await {
        Ok(devices) => {
            if devices.is_empty() {
                return result_html(
                    true,
                    "",
                    "<p class=\"muted\">No known Bluetooth devices.</p>",
                );
            }
            let mut html = String::from(
                r#"<table class="tools-table"><thead><tr><th>Name</th><th>MAC</th><th>State</th><th>Actions</th></tr></thead><tbody>"#,
            );
            for d in &devices {
                let name = d.name.clone().unwrap_or_else(|| "(unnamed)".to_string());
                let mut flags = Vec::new();
                if d.paired {
                    flags.push("paired");
                }
                if d.connected {
                    flags.push("connected");
                }
                if d.trusted {
                    flags.push("trusted");
                }
                html.push_str(&format!(
                    r#"<tr><td>{name}</td><td>{mac}</td><td class="muted">{state}</td><td>"#,
                    name = esc(&name),
                    mac = esc(&d.mac),
                    state = esc(&flags.join(" ")),
                ));
                for action in BT_ACTIONS {
                    html.push_str(&format!(
                        r##"<form hx-post="/devices/network/bt/action" hx-disabled-elt="find button" hx-target="#network-result" hx-swap="innerHTML" class="inline-form">
                             <input type="hidden" name="mac" value="{mac}">
                             <input type="hidden" name="action" value="{action}">
                             <button class="btn-mutate" type="submit">{action}</button>
                           </form>"##,
                        mac = esc(&d.mac),
                        action = action,
                    ));
                }
                html.push_str("</td></tr>");
            }
            html.push_str("</tbody></table>");
            result_html(true, "", &html)
        }
        Err(e) => error_result(&e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct BtActionForm {
    mac: String,
    action: String,
}

/// `POST /devices/network/bt/action` — `bt-<action> <mac>` for `action` in
/// [`BT_ACTIONS`].
pub async fn bt_action(
    State(state): State<SharedState>,
    Form(form): Form<BtActionForm>,
) -> impl IntoResponse {
    Html(render_bt_action(&state, &form.mac, &form.action).await)
}

pub async fn render_bt_action(state: &AppState, mac: &str, action: &str) -> String {
    if !BT_ACTIONS.contains(&action) {
        return error_result(&format!("unknown bluetooth action {action:?}"));
    }
    match validate_token(mac) {
        Ok(m) => run_line(state, &format!("bt-{action} {m}")).await,
        Err(msg) => error_result(&msg),
    }
}
