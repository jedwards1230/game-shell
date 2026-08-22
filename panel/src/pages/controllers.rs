//! `/devices/controllers` — the gamepad fleet: pads (battery/rumble/test), grab-state
//! management (`grab`/`release`/`handoff`), a button-binding editor
//! (`get-bindings`/`set-binding`/`capture-next`/`capture-cancel`), read-only
//! per-game/per-player binding layers, `set-active-game`, the controller
//! database (`controllerdb-status`/`-refresh`), a collapsed diagnostic
//! `list-input-devices` enumerator, and — since `docs/PANEL_IA.md` phase 3 —
//! the `Input` slice of `settings.json` (`controllerDebug`, `rumbleEnabled`).
//!
//! That last form posts to `POST /devices/controllers/settings/save`, which is
//! registered in `main.rs`'s `Gate::SettingsStore` block while this page is in
//! the `Gate::Controllers` one — a block condition may not AND two
//! capabilities. So the form renders only when
//! `caps.allows(Gate::SettingsStore)`; the panel never draws a control for a
//! route that was not registered.
//!
//! Degradation: `GET /controllers` gathers every section independently (like
//! `pages::processes`) — one section's IPC failure shows an inline
//! "unavailable" note without blanking the others; every action form is
//! always rendered (like `pages::tools`) and reports its own daemon-
//! unreachable error inline rather than failing page load. The route itself
//! is always 200, never a 500.

use std::collections::HashMap;
use std::time::Duration;

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;
use serde_json::Value;

use crate::capabilities::{Chrome, Gate};
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page's settings form owns — rendered as its
/// `__group` companions AND enforced server-side in [`save_settings`].
const OWNED: &[&str] = &["Input"];

// ---------------------------------------------------------------------------
// Fixed vocabularies (docs/IPC_PROTOCOL.md § `set-binding` / Remappable
// Buttons / Default Button Mappings).
// ---------------------------------------------------------------------------

const ACTIONS: &[&str] = &["select", "back", "altSelect", "confirm"];
const BUTTONS: &[&str] = &[
    "BTN_SOUTH",
    "BTN_EAST",
    "BTN_NORTH",
    "BTN_WEST",
    "BTN_TL",
    "BTN_TR",
    "BTN_SELECT",
    "BTN_START",
    "BTN_MODE",
    "BTN_THUMBL",
    "BTN_THUMBR",
];
/// `capture-next` blocks up to 10s server-side waiting for a button press
/// (`docs/IPC_PROTOCOL.md` § `capture-next`) — give the client enough
/// headroom over [`crate::ipc::IpcTransport`]'s 3s default so we receive that
/// reply instead of timing out first.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(12);
/// Bounds for the rumble-test `ms` field — long enough to feel, capped well
/// short of anything that reads as a malfunction.
const RUMBLE_MS_MIN: u64 = 20;
const RUMBLE_MS_MAX: u64 = 3000;

// ---------------------------------------------------------------------------
// Shared small helpers — each page keeps its own copy of this trio rather
// than a shared utility module (mirrors `pages::tools`/`pages::processes`/
// `pages::dev`, per the file-ownership contract).
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn pretty_json_text(s: &str) -> String {
    match serde_json::from_str::<Value>(s) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.to_string()),
        Err(_) => s.to_string(),
    }
}

fn pretty_json_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(val) => serde_json::to_string_pretty(val).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

/// Reject empty, whitespace, or control-character tokens — every
/// user-supplied argument that becomes part of an IPC command line (pad ids,
/// game ids) goes through this.
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
// Generic action result partial — shared `#controllers-result` target.
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "controllers_result.html")]
struct ControllersResultTemplate {
    ok: bool,
    body_html: String,
}

fn result_html(ok: bool, body_html: &str) -> String {
    let tmpl = ControllersResultTemplate {
        ok,
        body_html: body_html.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn error_result(msg: &str) -> String {
    result_html(false, &format!("<pre>{}</pre>", esc(msg)))
}

fn pretty_block(reply: &str) -> String {
    format!("<pre>{}</pre>", esc(&pretty_json_text(reply)))
}

/// Send `line` and render the reply into the shared result panel.
async fn run_line(state: &AppState, line: &str) -> String {
    match state.node.command(line).await {
        Ok(reply) => result_html(true, &pretty_block(&reply)),
        Err(e) => error_result(&e.to_string()),
    }
}

/// Like [`run_line`], but bolts on an out-of-band `#controllers-fleet`
/// refresh (see [`render_fleet_section`]) — used by grab/release/handoff,
/// whose whole point is changing the fleet's grab state, so the Fleet
/// section should never need a manual reload to reflect it.
async fn run_line_refresh_fleet(state: &AppState, line: &str) -> String {
    let result = run_line(state, line).await;
    format!("{result}{}", render_fleet_section(state, true).await)
}

// ---------------------------------------------------------------------------
// Bindings section — its own `#controllers-bindings` partial, re-rendered
// after every bindings-affecting action so the table always reflects the
// daemon's current state.
// ---------------------------------------------------------------------------

struct BindingView {
    action: &'static str,
    current: String,
    /// Pre-rendered `<option>` tags (already escaped) for the button
    /// `<select>` — avoids fragile string-equality checks in the template.
    select_html: String,
}

fn build_binding_views(map: &HashMap<String, String>) -> Vec<BindingView> {
    ACTIONS
        .iter()
        .map(|&action| {
            let current = map
                .get(action)
                .cloned()
                .unwrap_or_else(|| "(unset)".to_string());
            let mut opts = String::new();
            for b in BUTTONS {
                let sel = if *b == current { " selected" } else { "" };
                opts.push_str(&format!(r#"<option value="{b}"{sel}>{b}</option>"#));
            }
            BindingView {
                action,
                current,
                select_html: opts,
            }
        })
        .collect()
}

#[derive(Template)]
#[template(path = "controllers_bindings.html")]
struct BindingsTemplate {
    bindings: Vec<BindingView>,
    bindings_error: String,
    msg_ok: bool,
    msg: String,
}

/// Fetch fresh bindings and render the section, optionally with a result
/// banner (`ok`, message) from whatever action triggered the re-render.
async fn render_bindings_section(state: &AppState, msg: Option<(bool, String)>) -> String {
    let (bindings, bindings_error) = match state
        .node
        .command_json::<HashMap<String, String>>("get-bindings")
        .await
    {
        Ok(map) => (build_binding_views(&map), String::new()),
        Err(e) => (Vec::new(), e.to_string()),
    };
    let (msg_ok, msg) = msg.unwrap_or((true, String::new()));
    let tmpl = BindingsTemplate {
        bindings,
        bindings_error,
        msg_ok,
        msg,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

/// `POST /controllers/bindings/set` — `set-binding <action> <button>`,
/// validated against [`ACTIONS`]/[`BUTTONS`] before it ever reaches the
/// daemon. Always re-renders the bindings section (success or failure) so
/// the table reflects the current state.
pub async fn bindings_set(
    State(state): State<SharedState>,
    Form(form): Form<SetBindingForm>,
) -> impl IntoResponse {
    Html(render_set_binding(&state, &form.action, &form.button).await)
}

#[derive(Deserialize)]
pub struct SetBindingForm {
    action: String,
    button: String,
}

pub async fn render_set_binding(state: &AppState, action: &str, button: &str) -> String {
    if !ACTIONS.contains(&action) {
        return render_bindings_section(state, Some((false, format!("unknown action {action:?}"))))
            .await;
    }
    if !BUTTONS.contains(&button) {
        return render_bindings_section(
            state,
            Some((
                false,
                format!(
                    "unknown button {button:?} (allowed: {})",
                    BUTTONS.join(", ")
                ),
            )),
        )
        .await;
    }
    match state
        .node
        .command(&format!("set-binding {action} {button}"))
        .await
    {
        Ok(_) => render_bindings_section(state, Some((true, format!("{action} → {button}")))).await,
        Err(e) => render_bindings_section(state, Some((false, e.to_string()))).await,
    }
}

#[derive(Deserialize)]
pub struct ActionForm {
    action: String,
}

/// `POST /controllers/bindings/capture` — waits (up to 10s server-side) for
/// the next remappable button press via `capture-next`, then automatically
/// applies it with `set-binding <action> <button>`. "Keep it simple": one
/// button triggers the whole capture→bind flow; [`bindings_capture_cancel`]
/// unblocks a pending capture from a second, concurrent request.
pub async fn bindings_capture(
    State(state): State<SharedState>,
    Form(form): Form<ActionForm>,
) -> impl IntoResponse {
    Html(render_capture(&state, &form.action).await)
}

pub async fn render_capture(state: &AppState, action: &str) -> String {
    if !ACTIONS.contains(&action) {
        return render_bindings_section(state, Some((false, format!("unknown action {action:?}"))))
            .await;
    }
    match state
        .node
        .command_timeout("capture-next", CAPTURE_TIMEOUT)
        .await
    {
        Ok(reply) if reply == "timeout" => {
            render_bindings_section(
                state,
                Some((
                    false,
                    "capture timed out after 10s — no button was pressed".to_string(),
                )),
            )
            .await
        }
        Ok(reply) if reply == "cancelled" => {
            render_bindings_section(state, Some((false, "capture cancelled".to_string()))).await
        }
        Ok(reply) => match reply.strip_prefix("captured:") {
            Some(button) => match state
                .node
                .command(&format!("set-binding {action} {button}"))
                .await
            {
                Ok(_) => {
                    render_bindings_section(
                        state,
                        Some((true, format!("captured {button} → bound to {action}"))),
                    )
                    .await
                }
                Err(e) => {
                    render_bindings_section(
                        state,
                        Some((false, format!("captured {button} but binding failed: {e}"))),
                    )
                    .await
                }
            },
            None => {
                render_bindings_section(
                    state,
                    Some((false, format!("unexpected capture reply: {reply}"))),
                )
                .await
            }
        },
        Err(e) => render_bindings_section(state, Some((false, e.to_string()))).await,
    }
}

/// `POST /controllers/bindings/capture-cancel` — `capture-cancel`, unblocks
/// a pending [`render_capture`] request waiting on the daemon.
pub async fn bindings_capture_cancel(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_capture_cancel(&state).await)
}

pub async fn render_capture_cancel(state: &AppState) -> String {
    match state.node.command("capture-cancel").await {
        Ok(_) => {
            render_bindings_section(state, Some((true, "capture cancel requested".to_string())))
                .await
        }
        Err(e) => render_bindings_section(state, Some((false, e.to_string()))).await,
    }
}

// ---------------------------------------------------------------------------
// Page shell — GET /controllers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Pad {
    id: String,
    index: u32,
    name: String,
    grabbed: bool,
}

struct PadView {
    id: String,
    index: u32,
    name: String,
    grabbed: bool,
}

// ---------------------------------------------------------------------------
// Fleet section — its own `#controllers-fleet` partial (mirrors the bindings
// section above) so grab/release/handoff can refresh it out-of-band without
// touching the rest of the page.
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "controllers_fleet.html")]
struct FleetTemplate {
    oob: bool,
    status_label: String,
    status_dot_class: &'static str,
    status_raw: String,
    pads: Vec<PadView>,
    pads_error: String,
}

/// Fetch fresh `status`/`get-pads` and render the Fleet section. `oob` adds
/// `hx-swap-oob="true"` to the section tag for bolting this onto another
/// action's response (see [`run_line_refresh_fleet`]); `false` for the
/// section's own place in the normal page render.
async fn render_fleet_section(state: &AppState, oob: bool) -> String {
    let status_res = state.node.command("status").await;
    let pads_res = state.node.command_json::<Vec<Pad>>("get-pads").await;

    let (status_label, status_dot_class, status_raw) = match &status_res {
        Ok(s) => match crate::humanize::humanize_status(s) {
            Some(h) => (h.label, h.dot_class, h.raw),
            None => (s.clone(), "dot-warn", s.clone()),
        },
        Err(e) => (e.to_string(), "dot-error", String::new()),
    };

    let (pads, pads_error) = match pads_res {
        Ok(list) => (
            list.into_iter()
                .map(|p| PadView {
                    id: p.id,
                    index: p.index,
                    name: p.name,
                    grabbed: p.grabbed,
                })
                .collect(),
            String::new(),
        ),
        Err(e) => (Vec::new(), e.to_string()),
    };

    let tmpl = FleetTemplate {
        oob,
        status_label,
        status_dot_class,
        status_raw,
        pads,
        pads_error,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

#[derive(Template)]
#[template(path = "controllers.html")]
struct ControllersTemplate {
    chrome: Chrome,
    fleet_section_html: String,
    bindings_section_html: String,
    per_game_json: String,
    per_player_json: String,
    config_error: String,
    controllerdb_text: String,
    /// `POST /devices/controllers/settings/save` and `/shell/advanced` are
    /// registered — never render a control pointing at either otherwise.
    settings_enabled: bool,
    /// One hidden `__group` input per entry — see `settings::build_patch`.
    settings_scope: Vec<&'static str>,
    settings_groups: Vec<GroupView>,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let config_res = state.node.get_config().await;
    let controllerdb_res = state.node.command("controllerdb-status").await;

    let (per_game_json, per_player_json, config_error) = match &config_res {
        Ok(cfg) => (
            pretty_json_or_empty(cfg.get("perGameBindings")),
            pretty_json_or_empty(cfg.get("perPlayerBindings")),
            String::new(),
        ),
        Err(e) => (String::new(), String::new(), e.to_string()),
    };

    let controllerdb_text = match controllerdb_res {
        Ok(s) => pretty_json_text(&s),
        Err(e) => e.to_string(),
    };

    let fleet_section_html = render_fleet_section(state, false).await;
    let bindings_section_html = render_bindings_section(state, None).await;

    let settings_enabled = state.caps.allows(Gate::SettingsStore);
    let settings_groups = match (settings_enabled, &config_res) {
        // Reuses the `get-config` this page already made — the Input group is
        // two booleans out of the same document the binding layers come from.
        (true, Ok(cfg)) => settings::build_groups(cfg, OWNED),
        _ => Vec::new(),
    };

    let tmpl = ControllersTemplate {
        chrome: Chrome::new(&state.caps, "devices.controllers"),
        fleet_section_html,
        bindings_section_html,
        per_game_json,
        per_player_json,
        config_error,
        controllerdb_text,
        settings_enabled,
        settings_scope: OWNED.to_vec(),
        settings_groups,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// POST /devices/controllers/settings/save — the Input settings group
// ---------------------------------------------------------------------------

/// `POST /devices/controllers/settings/save` — the `Input` group of
/// `settings.json` (`controllerDebug`, `rumbleEnabled`) and nothing else.
///
/// Registered in `main.rs`'s `Gate::SettingsStore` block, not the
/// `Gate::Controllers` one that carries this page: a block condition may name
/// only one capability, and `set-config` is the one this route actually needs.
pub async fn save_settings(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(settings::render_save(&state, OWNED, &pairs).await)
}

// ---------------------------------------------------------------------------
// Grab management
// ---------------------------------------------------------------------------

pub async fn grab(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_grab(&state).await)
}

pub async fn render_grab(state: &AppState) -> String {
    run_line_refresh_fleet(state, "grab").await
}

pub async fn release(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_release(&state).await)
}

pub async fn render_release(state: &AppState) -> String {
    run_line_refresh_fleet(state, "release").await
}

pub async fn handoff(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_handoff(&state).await)
}

pub async fn render_handoff(state: &AppState) -> String {
    run_line_refresh_fleet(state, "handoff").await
}

// ---------------------------------------------------------------------------
// Per-pad battery / rumble
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct IdForm {
    id: String,
}

pub async fn pad_battery(
    State(state): State<SharedState>,
    Form(form): Form<IdForm>,
) -> impl IntoResponse {
    Html(render_pad_battery(&state, &form.id).await)
}

pub async fn render_pad_battery(state: &AppState, id: &str) -> String {
    match validate_token(id) {
        Ok(v) => run_line(state, &format!("pad-battery {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

pub async fn pad_rumble_status(
    State(state): State<SharedState>,
    Form(form): Form<IdForm>,
) -> impl IntoResponse {
    Html(render_pad_rumble_status(&state, &form.id).await)
}

pub async fn render_pad_rumble_status(state: &AppState, id: &str) -> String {
    match validate_token(id) {
        Ok(v) => run_line(state, &format!("pad-rumble-status {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

#[derive(Deserialize)]
pub struct RumbleForm {
    id: String,
    ms: String,
}

/// `POST /controllers/pad/rumble` — `rumble <id> <ms>`, `ms` clamped to
/// [`RUMBLE_MS_MIN`]..=[`RUMBLE_MS_MAX`] server-side (defense in depth beyond
/// the form's own `min`/`max` attributes).
pub async fn pad_rumble(
    State(state): State<SharedState>,
    Form(form): Form<RumbleForm>,
) -> impl IntoResponse {
    Html(render_pad_rumble(&state, &form.id, &form.ms).await)
}

pub async fn render_pad_rumble(state: &AppState, id: &str, ms: &str) -> String {
    let v = match validate_token(id) {
        Ok(v) => v,
        Err(msg) => return error_result(&msg),
    };
    let ms_n: u64 = match ms.trim().parse() {
        Ok(n) => n,
        Err(_) => return error_result(&format!("ms must be a non-negative integer: {ms:?}")),
    };
    if !(RUMBLE_MS_MIN..=RUMBLE_MS_MAX).contains(&ms_n) {
        return error_result(&format!(
            "ms must be between {RUMBLE_MS_MIN} and {RUMBLE_MS_MAX}"
        ));
    }
    run_line(state, &format!("rumble {v} {ms_n}")).await
}

// ---------------------------------------------------------------------------
// Diagnostics: list-input-devices (collapsed section on the page)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct InputDevice {
    name: String,
    path: String,
    vendor: String,
    product: String,
    #[allow(dead_code)]
    phys: String,
    handlers: Vec<String>,
    grabbed: bool,
}

pub async fn input_devices(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_input_devices(&state).await)
}

pub async fn render_input_devices(state: &AppState) -> String {
    match state
        .node
        .command_json::<Vec<InputDevice>>("list-input-devices")
        .await
    {
        Ok(devices) => {
            if devices.is_empty() {
                return result_html(
                    true,
                    "<p class=\"muted\">No controller-like input devices found.</p>",
                );
            }
            let mut html = String::from(
                r#"<table class="tools-table"><thead><tr><th>Name</th><th>Path</th><th>Vendor:Product</th><th>Handlers</th><th>Grabbed</th></tr></thead><tbody>"#,
            );
            for d in &devices {
                html.push_str(&format!(
                    "<tr><td>{name}</td><td>{path}</td><td class=\"muted\">{vid}:{pid}</td><td class=\"muted\">{handlers}</td><td>{grabbed}</td></tr>",
                    name = esc(&d.name),
                    path = esc(&d.path),
                    vid = esc(&d.vendor),
                    pid = esc(&d.product),
                    handlers = esc(&d.handlers.join(", ")),
                    grabbed = if d.grabbed { "yes" } else { "no" },
                ));
            }
            html.push_str("</tbody></table>");
            result_html(true, &html)
        }
        Err(e) => error_result(&e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Active game
// ---------------------------------------------------------------------------

/// `POST /controllers/active-game/set` — `set-active-game <id>`.
pub async fn active_game_set(
    State(state): State<SharedState>,
    Form(form): Form<IdForm>,
) -> impl IntoResponse {
    Html(render_active_game_set(&state, &form.id).await)
}

pub async fn render_active_game_set(state: &AppState, id: &str) -> String {
    match validate_token(id) {
        Ok(v) => run_line(state, &format!("set-active-game {v}")).await,
        Err(msg) => error_result(&msg),
    }
}

/// `POST /controllers/active-game/clear` — bare `set-active-game` (no body),
/// which the daemon treats as "clear the active game".
pub async fn active_game_clear(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_active_game_clear(&state).await)
}

pub async fn render_active_game_clear(state: &AppState) -> String {
    run_line(state, "set-active-game").await
}

// ---------------------------------------------------------------------------
// Controller DB
// ---------------------------------------------------------------------------

pub async fn controllerdb_status(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_controllerdb_status(&state).await)
}

pub async fn render_controllerdb_status(state: &AppState) -> String {
    run_line(state, "controllerdb-status").await
}

pub async fn controllerdb_refresh(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_controllerdb_refresh(&state).await)
}

pub async fn render_controllerdb_refresh(state: &AppState) -> String {
    run_line(state, "controllerdb-refresh").await
}
