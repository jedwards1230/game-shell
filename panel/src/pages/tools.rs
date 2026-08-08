//! `/tools` — a console over the daemon's read/act IPC surface, grouped by
//! domain (Navigation, Apps, Bluetooth, Network, Power, System), plus a raw
//! escape hatch that sends any single IPC line. Every action's response
//! renders into the page's shared `#tools-result` panel.
//!
//! Scope: CEC and controller/pads/bindings commands are NOT here — those are
//! M4's Controllers/CEC pages (`pages::controllers`, `pages::cec`), which
//! still render their M1 stub.
//!
//! Degradation: `GET /tools` never touches the daemon at load (no page-load
//! IPC call, unlike Dashboard/Settings/Widgets), so it's always 200 with the
//! full console rendered. Each action reports its own daemon-unreachable
//! error inline in the result panel — never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;

use crate::capabilities::Chrome;
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

// ---------------------------------------------------------------------------
// Fixed vocabularies (also drive the page's quick-action buttons)
// ---------------------------------------------------------------------------

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
const KEY_VOCAB: &[&str] = &["up", "down", "left", "right", "select", "back"];
const BT_ACTIONS: &[&str] = &["connect", "disconnect", "pair", "trust"];
/// Commands that belong to another page's guarded flow (Settings/Controllers)
/// — still allowed through the raw console, but with a warning, since sending
/// them here bypasses that page's own validation/UI.
const WARN_COMMANDS: &[&str] = &["set-config", "set-binding", "grab", "release", "handoff"];

// ---------------------------------------------------------------------------
// Page shell
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "tools.html")]
struct ToolsTemplate {
    chrome: Chrome,
    intent_quick: &'static [&'static str],
    overlay_quick: &'static [&'static str],
    settings_slugs: &'static [&'static str],
    key_quick: &'static [&'static str],
    /// `[panel].allow_dangerous` (S5) — gates the raw IPC console, which
    /// drives the whole IPC vocabulary and is therefore an arbitrary-command
    /// escape hatch. `POST /tools/raw` is not registered when this is false.
    allow_dangerous: bool,
}

/// `GET /tools` — the console. No IPC calls on load; every command is fired
/// by an htmx action.
pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    super::render(ToolsTemplate {
        chrome: Chrome::new(&state.caps, "tools"),
        intent_quick: INTENT_QUICK,
        overlay_quick: OVERLAY_QUICK,
        settings_slugs: SETTINGS_SLUGS,
        key_quick: KEY_VOCAB,
        allow_dangerous: state.cfg.allow_dangerous,
    })
}

// ---------------------------------------------------------------------------
// Shared result rendering
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "tools_result.html")]
struct ToolsResultTemplate {
    ok: bool,
    warning: String,
    body_html: String,
}

fn result_html(ok: bool, warning: &str, body_html: &str) -> String {
    let tmpl = ToolsResultTemplate {
        ok,
        warning: warning.to_string(),
        body_html: body_html.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn error_result(msg: &str) -> String {
    result_html(false, "", &format!("<pre>{}</pre>", esc(msg)))
}

/// Send `line` over IPC and render the reply: pretty-printed JSON when the
/// reply parses as JSON, the bare text otherwise. A `TransportError` (including
/// daemon-unreachable) renders as a failed result, never a 500.
pub async fn run_line(state: &AppState, line: &str) -> String {
    match state.node.command(line).await {
        Ok(reply) => result_html(true, "", &pretty_block(&reply)),
        Err(e) => error_result(&e.to_string()),
    }
}

fn pretty_block(reply: &str) -> String {
    let text = match serde_json::from_str::<serde_json::Value>(reply) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| reply.to_string()),
        Err(_) => reply.to_string(),
    };
    format!("<pre>{}</pre>", esc(&text))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Reject empty, whitespace, or control-character tokens — every
/// user-supplied argument that becomes part of an IPC command line (intent
/// names, wm_class, MAC addresses, interface names, ping hosts) goes through
/// this. Returns the trimmed token on success.
fn validate_token(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("value must not be empty".to_string());
    }
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "value {t:?} must not contain whitespace or control characters"
        ));
    }
    Ok(t.to_string())
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NameForm {
    name: String,
}

/// `POST /tools/intent` — free-text field and every quick/deep-link button
/// funnel through here (`name` is the intent's `<name>` argument).
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

/// `POST /tools/key` — the six-name closed vocabulary, validated server-side
/// too (defense in depth beyond the fixed button values).
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

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------

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

/// `POST /tools/apps/list` — `list-apps`, rendered with a per-app "launch"
/// button (`intent app:<wmClass>`).
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
                       <form hx-post="/tools/apps/launch" hx-disabled-elt="find button" hx-target="#tools-result" hx-swap="innerHTML" class="inline-form">
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

/// `POST /tools/apps/launch` — `intent app:<wm_class>`.
pub async fn launch_app(
    State(state): State<SharedState>,
    Form(form): Form<WmClassForm>,
) -> impl IntoResponse {
    Html(render_launch_app(&state, &form.wm_class).await)
}

async fn render_launch_app(state: &AppState, wm_class: &str) -> String {
    match validate_token(wm_class) {
        Ok(v) => run_line(state, &format!("intent app:{v}")).await,
        Err(msg) => error_result(&msg),
    }
}

/// `POST /tools/apps/recents` — `get-recents`.
pub async fn get_recents(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "get-recents").await)
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

/// `POST /tools/bt/list` — `bt-list`, rendered with per-device
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
                        r##"<form hx-post="/tools/bt/action" hx-disabled-elt="find button" hx-target="#tools-result" hx-swap="innerHTML" class="inline-form">
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

/// `POST /tools/bt/action` — `bt-<action> <mac>` for `action` in
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

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

pub async fn net_status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-status").await)
}

pub async fn net_wifi_list(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-wifi-list").await)
}

pub async fn net_wifi_rescan(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "net-wifi-rescan").await)
}

#[derive(Deserialize)]
pub struct IfaceForm {
    iface: String,
}

/// `POST /tools/net/throughput` — `net-throughput <iface>`. `iface` is
/// validated as a plain token with no path separators (it touches a sysfs
/// path on the daemon side).
pub async fn net_throughput(
    State(state): State<SharedState>,
    Form(form): Form<IfaceForm>,
) -> impl IntoResponse {
    Html(render_net_throughput(&state, &form.iface).await)
}

pub async fn render_net_throughput(state: &AppState, iface: &str) -> String {
    match validate_iface(iface) {
        Ok(v) => run_line(state, &format!("net-throughput {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

fn validate_iface(s: &str) -> Result<String, String> {
    let t = validate_token(s)?;
    if t.contains('/') || t.contains("..") {
        return Err(format!("invalid interface name {t:?}"));
    }
    Ok(t)
}

#[derive(Deserialize)]
pub struct PingForm {
    host: String,
    count: Option<String>,
}

/// `POST /tools/net/ping` — `net-ping <host> [count]`. `host` is validated as
/// a single token (no whitespace/control chars — it becomes an argv, not a
/// shell string, but a stray space would still split the daemon's own
/// whitespace-delimited command parsing); `count`, if given, must be an
/// integer in `1..=10` (the daemon clamps too, but reject out-of-range
/// input here rather than silently reinterpreting it).
pub async fn net_ping(
    State(state): State<SharedState>,
    Form(form): Form<PingForm>,
) -> impl IntoResponse {
    Html(render_net_ping(&state, &form.host, form.count.as_deref()).await)
}

pub async fn render_net_ping(state: &AppState, host: &str, count: Option<&str>) -> String {
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
// Power
// ---------------------------------------------------------------------------

pub async fn power_can_suspend(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "power-can-suspend").await)
}

pub async fn power_battery(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "power-battery").await)
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

pub async fn sys_status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "sys-status").await)
}

pub async fn sys_metrics(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "sys-metrics").await)
}

pub async fn sys_storage(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "storage-status").await)
}

pub async fn sys_build_info(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "build-info").await)
}

pub async fn controllerdb_status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "controllerdb-status").await)
}

pub async fn controllerdb_refresh(State(state): State<SharedState>) -> impl IntoResponse {
    Html(run_line(&state, "controllerdb-refresh").await)
}

// ---------------------------------------------------------------------------
// Raw console (escape hatch)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RawForm {
    cmd: String,
}

/// `POST /tools/raw` — sends any single IPC line as-is and shows the raw
/// reply. Rejects an empty line or one containing a newline/control
/// character (a smuggled second command). Commands in [`WARN_COMMANDS`]
/// (belonging to Settings'/Controllers' own guarded flows) are still sent,
/// with a warning banner on the result.
pub async fn raw(State(state): State<SharedState>, Form(form): Form<RawForm>) -> impl IntoResponse {
    Html(render_raw(&state, &form.cmd).await)
}

pub async fn render_raw(state: &AppState, cmd: &str) -> String {
    let line = cmd.trim();
    if line.is_empty() {
        return error_result("command must not be empty");
    }
    if line.chars().any(|c| c.is_control()) {
        return error_result("command must be a single line with no control characters");
    }
    let word = line.split_whitespace().next().unwrap_or("");
    let warning = if WARN_COMMANDS.contains(&word) {
        format!(
            "{word} belongs to another page's guarded flow (Settings/Controllers) — sending it \
             here bypasses that page's own validation/UI. Proceeding anyway."
        )
    } else {
        String::new()
    };
    match state.node.command(line).await {
        Ok(reply) => result_html(true, &warning, &pretty_block(&reply)),
        Err(e) => result_html(
            false,
            &warning,
            &format!("<pre>{}</pre>", esc(&e.to_string())),
        ),
    }
}
