//! `/devices/cec` — HDMI-CEC topology (`cec-scan`/`cec-device`), input switching
//! (`cec-active-source`, per-device `cec-power-on`/`-off`), the transmit-wedge
//! health/recovery flow (`cec-health`/`cec-test` plus an escalating "Recover
//! CEC" ladder: test → restart daemon → full reboot), and — since
//! `docs/PANEL_IA.md` phase 3 — the `CEC` slice of `settings.json`, so config
//! and actions finally share a page.
//!
//! **Two config surfaces, deliberately distinct.** `[cec].osd_name` is the
//! panel's one `config.toml` write (via `toml_edit`, `POST
//! /devices/cec/osd-name`); the session-lifecycle settings below it are
//! `settings.json` keys written through `set-config` (`POST
//! /devices/cec/config`). The latter route is registered in the
//! `Gate::SettingsStore` block while this page is in the `Gate::Cec` one — a
//! block condition cannot AND two capabilities — so the form is rendered only
//! when `caps.allows(Gate::SettingsStore)`.
//!
//! **Feature-gated.** CEC is Linux-only and requires the daemon to be built
//! `--features cec`; on a default build every `cec-*` command replies
//! `error:unsupported on this platform` (feature/platform off) or
//! `error:libcec unavailable` (adapter absent/wedged). Both map to an honest
//! "not available" banner here — never a raw failure banner — via
//! [`cec_unavailable_reason`].
//!
//! Degradation: `GET /cec` makes exactly one eager IPC call (`cec-health`,
//! explicitly read-only and cheap per `docs/IPC_PROTOCOL.md`); topology/scan
//! and switching are lazy, htmx-triggered, mirroring `pages::navigation`'s
//! no-page-load-IPC philosophy for anything that touches the bus. Always 200,
//! never a 500.

use askama::Template;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::Form;
use serde::Deserialize;
use serde_json::Value;

use crate::bridge::BridgeError;
use crate::capabilities::{Chrome, Gate};
use crate::pages::settings::{self, GroupView};
use crate::state::{AppState, SharedState};
use crate::transport::NodeTransportExt;

/// The `SettingField::group`s this page's settings form owns — rendered as its
/// `__group` companions AND enforced server-side in [`save_config`].
const OWNED: &[&str] = &["CEC"];

// ---------------------------------------------------------------------------
// Shared small helpers (own copy per page — see `pages::controllers`'s doc
// comment for why).
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

fn pretty_block(reply: &str) -> String {
    format!("<pre>{}</pre>", esc(&pretty_json_text(reply)))
}

#[derive(Template)]
#[template(path = "cec_result.html")]
struct CecResultTemplate {
    ok: bool,
    body_html: String,
}

fn result_html(ok: bool, body_html: &str) -> String {
    let tmpl = CecResultTemplate {
        ok,
        body_html: body_html.to_string(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

fn error_result(msg: &str) -> String {
    result_html(false, &format!("<pre>{}</pre>", esc(msg)))
}

/// Map the two well-known "CEC isn't there" error strings
/// (`docs/IPC_PROTOCOL.md` § HDMI-CEC Commands) to an honest, non-alarming
/// message. `None` for any other (genuine) failure, which callers still
/// render as a normal error.
fn cec_unavailable_reason(msg: &str) -> Option<&'static str> {
    if msg.contains("unsupported on this platform") {
        Some(
            "CEC not available in this daemon build (built without --features cec, \
             or running on a non-Linux host).",
        )
    } else if msg.contains("libcec unavailable") {
        Some(
            "CEC adapter unavailable right now — see the Health panel above for the \
             specific cause and recovery steps.",
        )
    } else {
        None
    }
}

/// Send a `cec-*` command and render the reply, mapping the two "CEC isn't
/// there" errors to an honest info banner instead of a failure banner.
/// Bolts on an out-of-band `#cec-health` refresh (see
/// [`oob_health_refresh`]) so the health panel stays current after any bus
/// action, not just the two ladder steps that already target it directly.
async fn run_cec(state: &AppState, line: &str) -> String {
    let result = match state.node.command(line).await {
        Ok(reply) => result_html(true, &pretty_block(&reply)),
        Err(e) => {
            let msg = e.to_string();
            match cec_unavailable_reason(&msg) {
                Some(friendly) => result_html(
                    true,
                    &format!("<p class=\"banner banner-warn\">{}</p>", esc(friendly)),
                ),
                None => error_result(&msg),
            }
        }
    };
    format!("{result}{}", oob_health_refresh(state).await)
}

/// `<addr>` must be a decimal logical address in `0..=15`
/// (`docs/IPC_PROTOCOL.md` § `cec-device`/`cec-power-on`/`cec-power-off`).
fn validate_addr(s: &str) -> Result<u8, String> {
    let n: i64 = s
        .trim()
        .parse()
        .map_err(|_| format!("addr must be an integer 0-15: {s:?}"))?;
    if !(0..=15).contains(&n) {
        return Err(format!("addr must be between 0 and 15 (got {n})"));
    }
    Ok(n as u8)
}

/// Friendly names for the 16 CEC logical addresses (CEC 1.4 device-type
/// table), used as a fallback when `cecDeviceNames` has no override for that
/// address. Mirrors what `AVControlSettings.qml` derives client-side.
fn default_device_name(addr: i64) -> &'static str {
    match addr {
        0 => "TV",
        1 => "Recorder 1",
        2 => "Recorder 2",
        3 => "Tuner 1",
        4 => "Playback 1",
        5 => "Audio System",
        6 => "Tuner 2",
        7 => "Tuner 3",
        8 => "Playback 2",
        9 => "Recorder 3",
        10 => "Tuner 4",
        11 => "Playback 3",
        12 => "Reserved 1",
        13 => "Reserved 2",
        14 => "Free Use",
        15 => "Unregistered/Broadcast",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Health & wedge recovery — its own `#cec-health` partial, re-rendered after
// Test / restart-daemon so the panel always reflects the current state.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CecHealthJson {
    transmit: String,
    reason: Option<String>,
    since: i64,
    #[serde(rename = "lastError")]
    last_error: Option<String>,
}

struct HealthView {
    banner_class: &'static str,
    headline: String,
    detail: String,
    recommend_test: bool,
    recommend_restart: bool,
    recommend_reboot: bool,
    /// `false` only for the `no_libcec` case (a build/platform gap, not a
    /// runtime fault) — hides the recovery buttons entirely in favor of a
    /// plain explanatory note, since restarting the daemon or rebooting the
    /// box can't fix a missing feature flag.
    cec_available: bool,
}

fn since_ago(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(ms);
    let delta_s = (now_ms - ms).max(0) / 1000;
    let ago = if delta_s < 60 {
        format!("{delta_s}s ago")
    } else if delta_s < 3600 {
        format!("{}m ago", delta_s / 60)
    } else {
        format!("{}h ago", delta_s / 3600)
    };
    format!(" (since {ago})")
}

/// The single next recovery step the health panel should highlight —
/// [`recommended_step`] chooses at most one, never more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RecommendedStep {
    Test,
    RestartDaemon,
    Reboot,
}

/// Choose the one recommended next step for a `(transmit, reason)` pair
/// (mirrors the state table in `docs/IPC_PROTOCOL.md` § `cec-health`).
/// Extracted as its own pure function — separate from
/// [`classify_health`]'s banner/headline text — so the mapping itself can be
/// unit-tested directly.
fn recommended_step(transmit: &str, reason: Option<&str>) -> Option<RecommendedStep> {
    match (transmit, reason) {
        ("ok", _) => None,
        // "transmit wedge": the adapter answers but every send fails —
        // restarting the daemon re-initializes libcec's connection, which is
        // what actually clears this state.
        ("failing", _) => Some(RecommendedStep::RestartDaemon),
        // Build/platform gap, not a runtime fault — no step can fix it.
        ("unavailable", Some("no_libcec")) => None,
        // No adapter found at all: a daemon restart re-opens the same
        // (absent) hardware, so it won't help — a full reboot re-inits the
        // USB/HDMI stack from scratch.
        ("unavailable", Some("no_adapter")) => Some(RecommendedStep::Reboot),
        // Adapter present but the open handshake failed — a daemon restart
        // is the direct fix.
        ("unavailable", Some("adapter_open_failed")) => Some(RecommendedStep::RestartDaemon),
        // Any other/unknown reason, or no transmit attempted yet: probe
        // first before escalating.
        ("unavailable", _) | (_, _) => Some(RecommendedStep::Test),
    }
}

/// Classify a `cec-health`/`cec-test` reply into a display-ready view.
/// Mirrors the `transmit`/`reason` state table in `docs/IPC_PROTOCOL.md`
/// § `cec-health`.
fn classify_health(h: &CecHealthJson) -> HealthView {
    let since_txt = since_ago(h.since);
    let step = recommended_step(&h.transmit, h.reason.as_deref());
    let recommend_test = step == Some(RecommendedStep::Test);
    let recommend_restart = step == Some(RecommendedStep::RestartDaemon);
    let recommend_reboot = step == Some(RecommendedStep::Reboot);
    match (h.transmit.as_str(), h.reason.as_deref()) {
        ("ok", _) => HealthView {
            banner_class: "banner-ok",
            headline: "CEC: healthy".to_string(),
            detail: format!("Last transmit succeeded{since_txt}."),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
        ("failing", _) => HealthView {
            banner_class: "banner-error",
            headline: "CEC: transmit wedge".to_string(),
            detail: format!(
                "The adapter can receive but every transmit is failing{since_txt}.{}",
                h.last_error
                    .as_deref()
                    .map(|e| format!(" Last error: {e}"))
                    .unwrap_or_default()
            ),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
        ("unavailable", Some("no_libcec")) => HealthView {
            banner_class: "banner-warn",
            headline: "CEC not available in this daemon build".to_string(),
            detail: "Built without --features cec, or running on a non-Linux host.".to_string(),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: false,
        },
        ("unavailable", Some("no_adapter")) => HealthView {
            banner_class: "banner-error",
            headline: "No CEC adapter detected".to_string(),
            detail: "libcec found zero adapters — check the physical HDMI-CEC adapter and cable, \
                      then re-test."
                .to_string(),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
        ("unavailable", Some("adapter_open_failed")) => HealthView {
            banner_class: "banner-error",
            headline: "CEC adapter not responding (hardware wedge)".to_string(),
            detail: "An adapter is present but the libcec open handshake failed. Re-seat the \
                      adapter, then restart the daemon."
                .to_string(),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
        ("unavailable", reason) => HealthView {
            banner_class: "banner-error",
            headline: "CEC unavailable".to_string(),
            detail: format!("reason: {}", reason.unwrap_or("unknown")),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
        (other, _) => HealthView {
            banner_class: "banner-warn",
            headline: format!("CEC: {other}"),
            detail: format!(
                "No transmit has been attempted yet{since_txt}. Run Test CEC to probe the bus."
            ),
            recommend_test,
            recommend_restart,
            recommend_reboot,
            cec_available: true,
        },
    }
}

#[cfg(test)]
mod recommendation_tests {
    use super::*;

    #[test]
    fn ok_recommends_nothing() {
        assert_eq!(recommended_step("ok", None), None);
    }

    #[test]
    fn failing_recommends_restart_daemon_only() {
        assert_eq!(
            recommended_step("failing", None),
            Some(RecommendedStep::RestartDaemon)
        );
    }

    #[test]
    fn no_libcec_recommends_nothing() {
        assert_eq!(recommended_step("unavailable", Some("no_libcec")), None);
    }

    #[test]
    fn no_adapter_recommends_reboot() {
        assert_eq!(
            recommended_step("unavailable", Some("no_adapter")),
            Some(RecommendedStep::Reboot)
        );
    }

    #[test]
    fn adapter_open_failed_recommends_restart_daemon() {
        assert_eq!(
            recommended_step("unavailable", Some("adapter_open_failed")),
            Some(RecommendedStep::RestartDaemon)
        );
    }

    #[test]
    fn unknown_unavailable_reason_recommends_test() {
        assert_eq!(
            recommended_step("unavailable", Some("something_else")),
            Some(RecommendedStep::Test)
        );
    }

    #[test]
    fn never_attempted_recommends_test() {
        assert_eq!(
            recommended_step("unknown", None),
            Some(RecommendedStep::Test)
        );
    }

    #[test]
    fn classify_health_marks_exactly_one_step_recommended() {
        for (transmit, reason) in [
            ("ok", None),
            ("failing", None),
            ("unavailable", Some("no_libcec")),
            ("unavailable", Some("no_adapter")),
            ("unavailable", Some("adapter_open_failed")),
            ("unavailable", Some("bogus")),
            ("unknown", None),
        ] {
            let h = CecHealthJson {
                transmit: transmit.to_string(),
                reason: reason.map(str::to_string),
                since: 0,
                last_error: None,
            };
            let view = classify_health(&h);
            let recommended_count = [
                view.recommend_test,
                view.recommend_restart,
                view.recommend_reboot,
            ]
            .into_iter()
            .filter(|&b| b)
            .count();
            assert!(
                recommended_count <= 1,
                "expected at most one recommended step for ({transmit:?}, {reason:?}), got {recommended_count}"
            );
        }
    }
}

#[derive(Template)]
#[template(path = "cec_health.html")]
struct CecHealthTemplate {
    view: HealthView,
    action_ok: bool,
    action_msg: String,
    /// `true` only when this render is an out-of-band refresh bolted onto
    /// another action's response (see [`oob_health_refresh`]) — adds
    /// `hx-swap-oob="true"` to the section tag. `false` for every render
    /// that IS the primary swap target (page load, Test CEC, restart-daemon)
    /// since those already point `hx-target="#cec-health"` directly.
    oob: bool,
}

/// Run `cmd` (`cec-health` or `cec-test`, same reply shape), classify it, and
/// render the health section, optionally with an `action_msg` banner from
/// whatever triggered the re-render (e.g. a restart-daemon attempt).
async fn render_health_view(
    state: &AppState,
    cmd: &str,
    action: Option<(bool, String)>,
    oob: bool,
) -> String {
    let view = match state.node.command_json::<CecHealthJson>(cmd).await {
        Ok(h) => classify_health(&h),
        Err(e) => HealthView {
            banner_class: "banner-error",
            headline: "CEC health check failed".to_string(),
            detail: e.to_string(),
            recommend_test: true,
            recommend_restart: false,
            recommend_reboot: false,
            cec_available: true,
        },
    };
    let (action_ok, action_msg) = action.unwrap_or((true, String::new()));
    let tmpl = CecHealthTemplate {
        view,
        action_ok,
        action_msg,
        oob,
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

async fn render_health_section(state: &AppState) -> String {
    render_health_view(state, "cec-health", None, false).await
}

/// A fresh `#cec-health` render as an htmx out-of-band swap fragment, meant
/// to be appended after another action's own response body. Every CEC action
/// below (scan/device/switching/power) bolts this on so the health panel
/// always reflects post-action state without a manual reload — the same
/// effect Test CEC/restart-daemon already get "for free" by targeting
/// `#cec-health` directly.
async fn oob_health_refresh(state: &AppState) -> String {
    render_health_view(state, "cec-health", None, true).await
}

/// `POST /cec/test` — step 1 of the recovery ladder: `cec-test`, a
/// side-effect-free poll probe that also refreshes the health state.
pub async fn test(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_test(&state).await)
}

pub async fn render_test(state: &AppState) -> String {
    render_health_view(state, "cec-test", None, false).await
}

/// `true` only when the bridge has no base URL / can't be reached at all —
/// as opposed to being reachable but the operation itself failing. Mirrors
/// `pages::dev`'s private `bridge_unavailable` (duplicated here rather than
/// imported — `pages::dev` is owned by M1, not this page).
fn bridge_unavailable(e: &BridgeError) -> bool {
    matches!(e, BridgeError::NotConfigured | BridgeError::Unreachable(_))
}

/// `POST /cec/recover/restart-daemon` — step 2 of the recovery ladder:
/// restart the input daemon (bridge first, falling back to direct exec, same
/// tier logic as `pages::dev::restart_daemon` — CEC re-initializes on daemon
/// start), then re-fetch and show the fresh health state.
pub async fn recover_restart_daemon(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_recover_restart_daemon(&state).await)
}

pub async fn render_recover_restart_daemon(state: &AppState) -> String {
    let (ok, text) = match state.bridge.restart_daemon().await {
        Ok(body) => (true, format!("Bridge — restart-daemon: ok\n{body}")),
        Err(e) if bridge_unavailable(&e) => match state.recovery.restart_daemon().await {
            Ok(body) => (true, format!("Direct exec — restart-daemon: ok\n{body}")),
            Err(e2) => (false, format!("Direct exec — restart-daemon failed: {e2}")),
        },
        Err(e) => (false, format!("Bridge — restart-daemon failed: {e}")),
    };
    let msg = if ok {
        format!(
            "{text}\n\nCEC re-initializes on daemon start — Test CEC in a few seconds to confirm."
        )
    } else {
        text
    };
    render_health_view(state, "cec-health", Some((ok, msg)), false).await
}

// ---------------------------------------------------------------------------
// Topology — GET-lazy `cec-scan` / `cec-device`, merged with the
// `cecDeviceNames` friendly-name overrides from `get-config`.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CecDeviceJson {
    #[serde(rename = "logicalAddress")]
    logical_address: i64,
    #[serde(rename = "powerStatus")]
    power_status: String,
}

fn friendly_name(names: &Value, addr: i64) -> String {
    names
        .get(addr.to_string())
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default_device_name(addr).to_string())
}

/// `POST /cec/scan` — `cec-scan`, rendered as a table with per-device
/// power-on/standby actions. Friendly names come from `cecDeviceNames`
/// (`get-config`), falling back to the CEC 1.4 device-type default.
pub async fn scan(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_scan(&state).await)
}

pub async fn render_scan(state: &AppState) -> String {
    let result = render_scan_result(state).await;
    format!("{result}{}", oob_health_refresh(state).await)
}

async fn render_scan_result(state: &AppState) -> String {
    let names = state
        .node
        .get_config()
        .await
        .ok()
        .and_then(|cfg| cfg.get("cecDeviceNames").cloned())
        .unwrap_or_else(|| Value::Object(Default::default()));

    match state
        .node
        .command_json::<Vec<CecDeviceJson>>("cec-scan")
        .await
    {
        Ok(devices) => {
            if devices.is_empty() {
                return result_html(
                    true,
                    "<p class=\"muted\">No CEC devices found on the bus.</p>",
                );
            }
            let mut html = String::from(
                r#"<table class="tools-table"><thead><tr><th>Addr</th><th>Name</th><th>Power</th><th>Actions</th></tr></thead><tbody>"#,
            );
            for d in &devices {
                let name = friendly_name(&names, d.logical_address);
                html.push_str(&format!(
                    "<tr><td>{addr}</td><td>{name}</td><td>{power}</td><td>\
                     <form hx-post=\"/cec/power-on\" hx-disabled-elt=\"find button\" hx-target=\"#cec-result\" hx-swap=\"innerHTML\" class=\"inline-form\" \
                     hx-confirm=\"Power on {name_attr} (addr {addr})?\">\
                     <input type=\"hidden\" name=\"addr\" value=\"{addr}\"><button class=\"btn-mutate\" type=\"submit\">Power on</button></form>\
                     <form hx-post=\"/cec/power-off\" hx-disabled-elt=\"find button\" hx-target=\"#cec-result\" hx-swap=\"innerHTML\" class=\"inline-form\" \
                     hx-confirm=\"Send standby to {name_attr} (addr {addr})?\">\
                     <input type=\"hidden\" name=\"addr\" value=\"{addr}\"><button class=\"btn-mutate\" type=\"submit\">Standby</button></form>\
                     </td></tr>",
                    addr = d.logical_address,
                    name = esc(&name),
                    power = esc(&d.power_status),
                    name_attr = esc(&name),
                ));
            }
            html.push_str("</tbody></table>");
            result_html(true, &html)
        }
        Err(e) => {
            let msg = e.to_string();
            match cec_unavailable_reason(&msg) {
                Some(friendly) => result_html(
                    true,
                    &format!("<p class=\"banner banner-warn\">{}</p>", esc(friendly)),
                ),
                None => error_result(&msg),
            }
        }
    }
}

#[derive(Deserialize)]
pub struct AddrForm {
    addr: String,
}

/// `POST /cec/device` — `cec-device <addr>`.
pub async fn device(
    State(state): State<SharedState>,
    Form(form): Form<AddrForm>,
) -> impl IntoResponse {
    Html(render_device(&state, &form.addr).await)
}

pub async fn render_device(state: &AppState, addr: &str) -> String {
    match validate_addr(addr) {
        Ok(a) => run_cec(state, &format!("cec-device {a}")).await,
        Err(msg) => error_result(&msg),
    }
}

// ---------------------------------------------------------------------------
// Switching
// ---------------------------------------------------------------------------

/// `POST /cec/active-source` — `cec-active-source`: the "switch input"
/// primitive (there is no separate switch-input command).
pub async fn active_source(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_active_source(&state).await)
}

pub async fn render_active_source(state: &AppState) -> String {
    run_cec(state, "cec-active-source").await
}

/// `POST /cec/power-on` — `cec-power-on <addr>`.
pub async fn power_on(
    State(state): State<SharedState>,
    Form(form): Form<AddrForm>,
) -> impl IntoResponse {
    Html(render_power_on(&state, &form.addr).await)
}

pub async fn render_power_on(state: &AppState, addr: &str) -> String {
    match validate_addr(addr) {
        Ok(a) => run_cec(state, &format!("cec-power-on {a}")).await,
        Err(msg) => error_result(&msg),
    }
}

/// `POST /cec/power-off` — `cec-power-off <addr>`.
pub async fn power_off(
    State(state): State<SharedState>,
    Form(form): Form<AddrForm>,
) -> impl IntoResponse {
    Html(render_power_off(&state, &form.addr).await)
}

pub async fn render_power_off(state: &AppState, addr: &str) -> String {
    match validate_addr(addr) {
        Ok(a) => run_cec(state, &format!("cec-power-off {a}")).await,
        Err(msg) => error_result(&msg),
    }
}

// ---------------------------------------------------------------------------
// OSD device name — the input label TVs/AVRs display for this machine.
// ---------------------------------------------------------------------------
//
// The daemon resolves it as `[cec].osd_name` from config.toml, else the
// machine hostname (daemon/src/daemon_config.rs `resolve_osd_name`); the
// panel mirrors that resolution for display and owns the config.toml write —
// its only config write, done format-preservingly via `toml_edit` so the
// operator's comments/sections survive. A daemon restart applies the change
// (libcec announces the name once, at connection open).

/// libcec's `strDeviceName` limit; mirrors `CEC_OSD_NAME_MAX` in the daemon.
const OSD_NAME_MAX: usize = 13;

/// The machine hostname via `gethostname(2)`, `None` on failure/empty.
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Read the current `[cec].osd_name` override out of config.toml, `None` when
/// the file/key is absent or unparseable (the daemon then uses the hostname).
fn read_osd_override() -> Option<String> {
    let text = std::fs::read_to_string(crate::config::config_toml_path()).ok()?;
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let name = doc.get("cec")?.get("osd_name")?.as_str()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Validate a submitted OSD name: trimmed; empty ⇒ `Ok(None)` (clear the
/// override, reverting to the hostname default); else printable-ASCII-only and
/// at most [`OSD_NAME_MAX`] chars.
fn validate_osd_name(input: &str) -> Result<Option<String>, String> {
    let name = input.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.len() > OSD_NAME_MAX {
        return Err(format!(
            "name is {} chars; the CEC OSD name limit is {OSD_NAME_MAX}",
            name.len()
        ));
    }
    if !name.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        return Err("name must be printable ASCII (letters, digits, - _ etc.)".to_string());
    }
    Ok(Some(name.to_string()))
}

/// Apply an OSD-name override to a config.toml document, preserving all other
/// content: `Some(name)` sets `[cec].osd_name`, `None` removes the key (and a
/// then-empty `[cec]` table stays — harmless and less surprising than deleting
/// a section the operator may have commented around). Uses `as_table_like_mut`
/// so a hand-written inline table (`cec = { lifecycle = true }`) is edited the
/// same as a standard `[cec]` section instead of silently no-opping.
fn apply_osd_name(doc_text: &str, name: Option<&str>) -> Result<String, String> {
    let mut doc = doc_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("config.toml parse failed: {e}"))?;
    match name {
        Some(n) => {
            let table = doc
                .entry("cec")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or_else(|| "[cec] exists but is not a table".to_string())?;
            table.insert("osd_name", toml_edit::value(n));
        }
        None => {
            if let Some(table) = doc.get_mut("cec").and_then(|i| i.as_table_like_mut()) {
                table.remove("osd_name");
            }
        }
    }
    Ok(doc.to_string())
}

/// Write `contents` to `path` atomically: a same-directory tempfile + rename,
/// carrying over the existing file's permissions when it exists. The daemon
/// refuses to start on an unparseable config.toml, so a torn in-place write
/// here (crash/power-loss between truncate and write) could brick the next
/// boot — the rename makes that window disappear.
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.panel-tmp");
    std::fs::write(&tmp, contents)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
}

#[derive(Deserialize)]
pub struct OsdNameForm {
    osd_name: String,
}

/// `POST /cec/osd-name` — validate and write the `[cec].osd_name` override to
/// config.toml (format-preserving), or clear it when the field is emptied.
/// The daemon announces the name at libcec-open, so the result points at the
/// Processes page to restart it.
pub async fn save_osd_name(
    State(_state): State<SharedState>,
    Form(form): Form<OsdNameForm>,
) -> impl IntoResponse {
    Html(render_save_osd_name(&form.osd_name))
}

pub fn render_save_osd_name(input: &str) -> String {
    let name = match validate_osd_name(input) {
        Ok(n) => n,
        Err(msg) => return result_html(false, &format!("Not saved: {}", esc(&msg))),
    };
    let path = crate::config::config_toml_path();
    let current = match std::fs::read_to_string(&path) {
        // Only a genuinely ABSENT file may start from an empty document —
        // any other read failure (EACCES, non-UTF-8 bytes, I/O error) must
        // abort, or we'd "save" a fresh config.toml over an existing one and
        // destroy every other section the operator has configured.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if name.is_none() {
                // Clearing with no file present is already the default state.
                return result_html(
                    true,
                    "No override set — the hostname default already applies.",
                );
            }
            String::new()
        }
        Err(e) => {
            return result_html(
                false,
                &format!(
                    "Not saved: could not read {}: {}",
                    path.display(),
                    esc(&e.to_string())
                ),
            )
        }
        Ok(t) => t,
    };
    let updated = match apply_osd_name(&current, name.as_deref()) {
        Ok(u) => u,
        Err(msg) => return result_html(false, &format!("Not saved: {}", esc(&msg))),
    };
    if let Err(e) = write_atomic(&path, &updated) {
        return result_html(
            false,
            &format!(
                "Write failed for {}: {}",
                path.display(),
                esc(&e.to_string())
            ),
        );
    }
    let effective = name.unwrap_or_else(default_osd_name);
    result_html(
        true,
        &format!(
            "Saved. CEC input name is now \"{}\" — restart tv-shell-input \
             (Processes page) to announce it on the bus.",
            esc(&effective)
        ),
    )
}

/// Reduce a candidate name exactly like the daemon's `resolve_osd_name` does:
/// printable-ASCII only, then truncated to [`OSD_NAME_MAX`]. Keeping the
/// filter identical matters — the panel displays the "effective" name, and it
/// must match what the daemon actually announces on the bus.
fn sanitize_osd_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(OSD_NAME_MAX)
        .collect()
}

/// The name the daemon falls back to without an override: the sanitized
/// hostname (mirroring the daemon's `resolve_osd_name`), else the historical
/// `"tv-shell"`.
fn default_osd_name() -> String {
    hostname()
        .map(|h| sanitize_osd_name(&h))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "tv-shell".to_string())
}

// ---------------------------------------------------------------------------
// Page shell — GET /cec
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "cec.html")]
struct CecTemplate {
    chrome: Chrome,
    health_section_html: String,
    /// The name the daemon will announce: override else hostname else fallback.
    osd_effective: String,
    /// Where the effective name comes from, for the source hint line.
    osd_source: &'static str,
    /// Current override value (input prefill; empty when hostname-defaulted).
    osd_configured: String,
    /// `POST /devices/cec/config` is registered — never render the form that
    /// posts to it otherwise.
    settings_enabled: bool,
    /// `get-config` answered, so the typed CEC form has real values to show.
    settings_up: bool,
    /// One hidden `__group` input per entry — see `settings::build_patch`.
    settings_scope: Vec<&'static str>,
    settings_groups: Vec<GroupView>,
}

pub async fn page(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_page(&state).await)
}

pub async fn render_page(state: &AppState) -> String {
    let health_section_html = render_health_section(state).await;
    let settings_enabled = state.caps.allows(Gate::SettingsStore);
    // Only fetch the settings document when the form that would show it is
    // actually going to render — a gated-off page must not add an IPC call.
    let cfg = if settings_enabled {
        state.node.get_config().await.ok()
    } else {
        None
    };
    let configured = read_osd_override();
    let (osd_effective, osd_source) = match &configured {
        Some(name) => (name.clone(), "config.toml [cec].osd_name override"),
        None => match hostname() {
            Some(h) => (
                sanitize_osd_name(&h),
                "hostname default (no [cec].osd_name override)",
            ),
            None => ("tv-shell".to_string(), "built-in fallback"),
        },
    };
    let tmpl = CecTemplate {
        chrome: Chrome::new(&state.caps, "devices.cec"),
        health_section_html,
        osd_effective,
        osd_source,
        osd_configured: configured.unwrap_or_default(),
        settings_enabled,
        settings_up: cfg.is_some(),
        settings_scope: OWNED.to_vec(),
        settings_groups: cfg
            .as_ref()
            .map(|c| settings::build_groups(c, OWNED))
            .unwrap_or_default(),
    };
    tmpl.render()
        .unwrap_or_else(|e| format!("<p class=\"banner banner-error\">render error: {e}</p>"))
}

// ---------------------------------------------------------------------------
// POST /devices/cec/config — the CEC settings group
// ---------------------------------------------------------------------------

/// `POST /devices/cec/config` — the `CEC` group of `settings.json` and nothing
/// else. Distinct from [`save_osd_name`], which writes `config.toml`.
///
/// Registered in `main.rs`'s `Gate::SettingsStore` block, not the `Gate::Cec`
/// one that carries this page: a block condition may name only one capability,
/// and `set-config` is the capability this route actually needs.
pub async fn save_config(
    State(state): State<SharedState>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    Html(settings::render_save(&state, OWNED, &pairs).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_osd_name_rules() {
        assert_eq!(validate_osd_name("  ").unwrap(), None); // empty ⇒ clear
        assert_eq!(
            validate_osd_name(" living-room ").unwrap(),
            Some("living-room".to_string())
        );
        assert!(validate_osd_name("a-name-way-too-long").is_err()); // > 13
        assert!(validate_osd_name("télé").is_err()); // non-ASCII
    }

    #[test]
    fn apply_osd_name_preserves_document() {
        let doc = "# my comment\n[http]\nbind = \"127.0.0.1:8089\"\n\n[cec]\nlifecycle = true\n";
        let set = apply_osd_name(doc, Some("htpc-1")).unwrap();
        assert!(set.contains("# my comment"), "comment must survive: {set}");
        assert!(set.contains("bind = \"127.0.0.1:8089\""));
        assert!(set.contains("lifecycle = true"));
        assert!(set.contains("osd_name = \"htpc-1\""));

        // Clearing removes only the key; the rest stays intact.
        let cleared = apply_osd_name(&set, None).unwrap();
        assert!(!cleared.contains("osd_name"));
        assert!(cleared.contains("lifecycle = true"));
        assert!(cleared.contains("# my comment"));
    }

    #[test]
    fn apply_osd_name_creates_cec_table_when_absent() {
        let out = apply_osd_name("", Some("htpc-1")).unwrap();
        assert!(out.contains("[cec]"));
        assert!(out.contains("osd_name = \"htpc-1\""));
        // Clearing on a doc with no [cec] table is a clean no-op.
        assert_eq!(apply_osd_name("", None).unwrap(), "");
    }

    #[test]
    fn apply_osd_name_handles_inline_cec_table() {
        // A hand-written inline table must be edited, not silently no-opped.
        let doc = "cec = { lifecycle = true }\n";
        let set = apply_osd_name(doc, Some("htpc-1")).unwrap();
        assert!(set.contains("lifecycle = true"));
        assert!(
            set.contains("osd_name = \"htpc-1\""),
            "set must land: {set}"
        );
        let cleared = apply_osd_name(&set, None).unwrap();
        assert!(
            !cleared.contains("osd_name"),
            "clear must remove: {cleared}"
        );
        assert!(cleared.contains("lifecycle = true"));
    }

    #[test]
    fn sanitize_matches_daemon_resolution() {
        // Mirrors daemon resolve_osd_name: ASCII filter BEFORE the 13-char cut.
        assert_eq!(sanitize_osd_name("héllo tv"), "hllo tv");
        assert_eq!(
            sanitize_osd_name("a-very-long-device-name"),
            "a-very-long-d"
        );
    }
}
